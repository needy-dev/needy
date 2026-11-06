mod arch;

use crate::{Action, Command, Exec, Outcome, Replacement, Report};
use libc::{c_void, pid_t, sock_filter, sock_fprog};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString};
use std::io::{Error, ErrorKind, Result};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_BYTES: usize = 2 << 20;
const MAX_STRINGS: usize = 32768;

#[cfg(target_env = "musl")]
type PtraceRequest = libc::c_int;
#[cfg(not(target_env = "musl"))]
type PtraceRequest = libc::c_uint;

pub(super) fn ptrace(op: PtraceRequest, pid: pid_t, address: usize, data: usize) -> Result<i64> {
    unsafe { *libc::__errno_location() = 0 };
    let result = unsafe { libc::ptrace(op, pid, address as *mut c_void, data as *mut c_void) };
    if result == -1 && unsafe { *libc::__errno_location() } != 0 {
        return Err(Error::last_os_error());
    }
    Ok(result)
}

fn resume(pid: pid_t, syscall: bool, signal: i32) -> Result<()> {
    let operation = if syscall {
        libc::PTRACE_SYSCALL
    } else {
        libc::PTRACE_CONT
    };
    match ptrace(operation, pid, 0, signal as usize) {
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        result => result.map(|_| ()),
    }
}

fn filter() -> Vec<sock_filter> {
    let instruction = |code, jt, jf, k| sock_filter { code, jt, jf, k };
    vec![
        instruction(0x20, 0, 0, 4), // seccomp_data.arch
        instruction(0x15, 1, 0, arch::AUDIT_ARCH),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        instruction(0x20, 0, 0, 0), // seccomp_data.nr
        instruction(0x15, 2, 0, libc::SYS_execve as u32),
        instruction(0x15, 1, 0, libc::SYS_execveat as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_TRACE),
    ]
}

fn c_string(value: &std::ffi::OsStr) -> Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "NUL in exec argument"))
}

fn resolve(program: &Path, cwd: &Path, environment: &[OsString]) -> Result<PathBuf> {
    if program.as_os_str().as_bytes().contains(&b'/') {
        return Ok(if program.is_absolute() {
            program.into()
        } else {
            cwd.join(program)
        });
    }
    let search = environment
        .iter()
        .find_map(|entry| entry.as_bytes().strip_prefix(b"PATH="));
    let search = search.ok_or_else(|| Error::new(ErrorKind::NotFound, "PATH is unset"))?;
    for directory in std::env::split_paths(&OsString::from_vec(search.to_vec())) {
        let candidate = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        }
        .join(program);
        let path = c_string(candidate.as_os_str())?;
        if unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0 && candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(Error::new(
        ErrorKind::NotFound,
        "executable not found in PATH",
    ))
}

enum Pending {
    Allocate {
        original: Box<arch::Registers>,
        replacement: Replacement,
        environment: Vec<OsString>,
    },
    Deny(i32),
    VforkReturn(Vec<(u64, u64)>),
    UnmapEntry {
        original: Box<arch::Registers>,
        mappings: Vec<(u64, u64)>,
    },
    UnmapExit {
        original: Box<arch::Registers>,
        mappings: Vec<(u64, u64)>,
    },
}

struct Task {
    parent: Option<pid_t>,
    first_root_exec: bool,
    internal: bool,
    newborn: bool,
    pending: Option<Pending>,
    vfork_parent: Option<pid_t>,
    mappings: Vec<(u64, u64)>,
}

struct Tree(HashMap<pid_t, Task>, HashSet<pid_t>);

impl Drop for Tree {
    fn drop(&mut self) {
        for pid in self.0.keys().chain(self.1.iter()) {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        // Only this OS thread's children/tracees belong to this supervisor.
        while !self.0.is_empty() || !self.1.is_empty() {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL | libc::__WNOTHREAD) };
            if pid < 0 {
                if Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                self.0.remove(&pid);
                self.1.remove(&pid);
            } else {
                let _ = resume(pid, false, libc::SIGKILL);
            }
        }
    }
}

/// See the crate-level platform and ownership contract.
pub fn supervise(
    command: Command,
    cancel: &AtomicBool,
    mut policy: impl FnMut(&Exec) -> Result<Action>,
) -> Result<Report> {
    let program =
        c_string(resolve(&command.program, &command.cwd, &command.environment)?.as_os_str())?;
    let cwd = c_string(command.cwd.as_os_str())?;
    let mut arguments = vec![c_string(command.program.as_os_str())?];
    arguments.extend(
        command
            .args
            .iter()
            .map(|arg| c_string(arg))
            .collect::<Result<Vec<_>>>()?,
    );
    let environment = command
        .environment
        .iter()
        .map(|entry| c_string(entry))
        .collect::<Result<Vec<_>>>()?;
    let mut argv: Vec<_> = arguments.iter().map(|value| value.as_ptr()).collect();
    let mut envp: Vec<_> = environment.iter().map(|value| value.as_ptr()).collect();
    argv.push(std::ptr::null());
    envp.push(std::ptr::null());
    let mut instructions = filter();
    let filter = sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };

    // All allocations are complete. The post-fork child only calls libc and
    // _exit until exec, so callers may have other threads in the parent.
    let root = unsafe { libc::fork() };
    if root < 0 {
        return Err(Error::last_os_error());
    }
    if root == 0 {
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            if libc::chdir(cwd.as_ptr()) != 0 || libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) == -1 {
                libc::_exit(126);
            }
            libc::raise(libc::SIGSTOP);
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &filter) != 0
            {
                libc::_exit(126);
            }
            libc::execve(program.as_ptr(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(if *libc::__errno_location() == libc::ENOENT {
                127
            } else {
                126
            });
        }
    }
    let mut tree = Tree(
        HashMap::from([(
            root,
            Task {
                parent: None,
                first_root_exec: true,
                internal: false,
                newborn: true,
                pending: None,
                vfork_parent: None,
                mappings: Vec::new(),
            },
        )]),
        HashSet::new(),
    );
    let options = libc::PTRACE_O_TRACESECCOMP
        | libc::PTRACE_O_TRACEFORK
        | libc::PTRACE_O_TRACEVFORK
        | libc::PTRACE_O_TRACEVFORKDONE
        | libc::PTRACE_O_TRACECLONE
        | libc::PTRACE_O_TRACEEXEC
        | libc::PTRACE_O_EXITKILL
        | libc::PTRACE_O_TRACESYSGOOD;
    let mut report = Report {
        root_pid: root,
        outcome: Outcome::Exited(126),
        exec_events: 0,
        redirected: 0,
        injection_stops: 0,
    };
    let mut root_exited = false;
    while !tree.0.is_empty() || !tree.1.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::new(ErrorKind::Interrupted, "process tree cancelled"));
        }
        let mut status = 0;
        let pid = unsafe {
            libc::waitpid(
                -1,
                &mut status,
                libc::__WALL | libc::__WNOTHREAD | libc::WNOHANG,
            )
        };
        if pid == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if pid < 0 {
            if Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(Error::last_os_error());
        }
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            tree.0.remove(&pid);
            tree.1.remove(&pid);
            if pid == root {
                root_exited = true;
                report.outcome = if libc::WIFEXITED(status) {
                    Outcome::Exited(libc::WEXITSTATUS(status))
                } else {
                    Outcome::Signaled(libc::WTERMSIG(status))
                };
            }
            continue;
        }
        let event = status >> 16;
        if event == libc::PTRACE_EVENT_EXEC {
            let mut former = 0u64;
            ptrace(
                libc::PTRACE_GETEVENTMSG,
                pid,
                0,
                (&mut former as *mut u64) as usize,
            )?;
            if former as i32 != pid
                && let Some(task) = tree.0.remove(&(former as i32))
            {
                tree.0.insert(pid, task);
            }
        }
        if !tree.0.contains_key(&pid) {
            if event != 0 || libc::WSTOPSIG(status) != libc::SIGSTOP {
                return Err(Error::other(format!(
                    "unexpected tracee pid={pid} status={status:#x}"
                )));
            }
            // A newborn's initial stop can precede its parent's fork event.
            // Hold it until that event gives us its parent's runtime role.
            tree.1.insert(pid);
            continue;
        }
        let task = tree.0.get_mut(&pid).unwrap();
        if task.newborn {
            task.newborn = false;
            ptrace(libc::PTRACE_SETOPTIONS, pid, 0, options as usize)?;
            resume(pid, false, 0)?;
            continue;
        }
        if event == libc::PTRACE_EVENT_FORK
            || event == libc::PTRACE_EVENT_VFORK
            || event == libc::PTRACE_EVENT_CLONE
        {
            let internal = task.internal;
            let mut child = 0u64;
            ptrace(
                libc::PTRACE_GETEVENTMSG,
                pid,
                0,
                (&mut child as *mut u64) as usize,
            )?;
            let already_stopped = tree.1.remove(&(child as i32));
            tree.0.insert(
                child as i32,
                Task {
                    parent: Some(pid),
                    first_root_exec: false,
                    internal,
                    newborn: !already_stopped,
                    pending: None,
                    vfork_parent: (event == libc::PTRACE_EVENT_VFORK).then_some(pid),
                    mappings: Vec::new(),
                },
            );
            if already_stopped {
                ptrace(libc::PTRACE_SETOPTIONS, child as i32, 0, options as usize)?;
                resume(child as i32, false, 0)?;
            }
            resume(pid, false, 0)?;
        } else if event == libc::PTRACE_EVENT_VFORK_DONE {
            if task.mappings.is_empty() {
                resume(pid, false, 0)?;
            } else {
                task.pending = Some(Pending::VforkReturn(std::mem::take(&mut task.mappings)));
                resume(pid, true, 0)?;
            }
        } else if event == libc::PTRACE_EVENT_SECCOMP {
            report.exec_events += 1;
            if task.first_root_exec || task.internal {
                task.first_root_exec = false;
                resume(pid, false, 0)?;
                continue;
            }
            let mut registers = arch::read(pid)?;
            let request = match decode(pid, task.parent, &registers) {
                Ok(request) => request,
                Err(error) if matches!(error.raw_os_error(), Some(libc::EFAULT | libc::EIO)) => {
                    // Invalid userspace pointers belong to the kernel's exec
                    // validation, rather than terminating the whole tree.
                    resume(pid, false, 0)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            match policy(&request)? {
                Action::Continue => resume(pid, false, 0)?,
                Action::Deny(errno) => {
                    if !(1..=4095).contains(&errno) {
                        return Err(Error::new(ErrorKind::InvalidInput, "invalid denial errno"));
                    }
                    arch::replace_pending(pid, &mut registers, -1, [0; 6])?;
                    task.pending = Some(Pending::Deny(errno));
                    resume(pid, true, 0)?;
                }
                Action::Redirect(replacement) => {
                    validate_replacement(&replacement)?;
                    let bytes = replacement_size(&replacement, &request.environment)?;
                    let original = Box::new(registers);
                    let mut mapped = arch::read(pid)?;
                    arch::replace_pending(
                        pid,
                        &mut mapped,
                        libc::SYS_mmap,
                        [
                            0,
                            bytes as u64,
                            (libc::PROT_READ | libc::PROT_WRITE) as u64,
                            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                            u64::MAX,
                            0,
                        ],
                    )?;
                    task.pending = Some(Pending::Allocate {
                        original,
                        replacement,
                        environment: request.environment,
                    });
                    resume(pid, true, 0)?;
                }
            }
        } else if libc::WSTOPSIG(status) == (libc::SIGTRAP | 0x80) {
            report.injection_stops += 1;
            let pending = task
                .pending
                .take()
                .ok_or_else(|| Error::other("unexpected syscall stop"))?;
            let mut registers = arch::read(pid)?;
            let mut single_step = false;
            match pending {
                Pending::Deny(errno) => {
                    arch::set_result(&mut registers, -(errno as i64));
                    arch::write(pid, &registers)?;
                }
                Pending::Allocate {
                    original,
                    replacement,
                    environment,
                } => {
                    let address = arch::result(&registers);
                    if (-4095..0).contains(&address) {
                        return Err(Error::from_raw_os_error(-address as i32));
                    }
                    let (bytes, argv, envp) = layout(address as u64, &replacement, &environment)?;
                    let map_length = replacement_size(&replacement, &environment)? as u64;
                    write_memory(pid, address as u64, &bytes)?;
                    let mut restored = *original;
                    arch::restart_exec(&mut restored, address as u64, argv, envp);
                    arch::write(pid, &restored)?;
                    task.internal = true;
                    let parent = task.vfork_parent;
                    report.redirected += 1;
                    if let Some(parent) = parent {
                        tree.0
                            .get_mut(&parent)
                            .ok_or_else(|| Error::other("vfork parent missing"))?
                            .mappings
                            .push((address as u64, map_length));
                    }
                }
                Pending::VforkReturn(mut mappings) => {
                    let (address, length) = mappings.pop().unwrap();
                    let original = Box::new(registers);
                    let mut call = arch::read(pid)?;
                    arch::restart_call(&mut call, libc::SYS_munmap, [address, length, 0, 0, 0, 0]);
                    arch::write(pid, &call)?;
                    task.pending = Some(Pending::UnmapEntry { original, mappings });
                    single_step = true;
                }
                Pending::UnmapEntry { original, mappings } => {
                    task.pending = Some(Pending::UnmapExit { original, mappings });
                    single_step = true;
                }
                Pending::UnmapExit {
                    original,
                    mut mappings,
                } => {
                    if arch::result(&registers) != 0 {
                        return Err(Error::other("cannot release shared vfork scratch mapping"));
                    }
                    if let Some((address, length)) = mappings.pop() {
                        arch::restart_call(
                            &mut registers,
                            libc::SYS_munmap,
                            [address, length, 0, 0, 0, 0],
                        );
                        arch::write(pid, &registers)?;
                        task.pending = Some(Pending::UnmapEntry { original, mappings });
                        single_step = true;
                    } else {
                        arch::write(pid, &original)?;
                    }
                }
            }
            resume(pid, single_step, 0)?;
        } else if event != 0 {
            resume(pid, false, 0)?;
        } else {
            // Ordinary application signals retain their native delivery.
            resume(pid, task.pending.is_some(), libc::WSTOPSIG(status))?;
        }
    }
    if !root_exited {
        return Err(Error::other("root exit status was not observed"));
    }
    Ok(report)
}

fn read_memory(pid: pid_t, address: u64, length: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(length);
    let word = std::mem::size_of::<usize>();
    while bytes.len() < length {
        let value = ptrace(
            libc::PTRACE_PEEKDATA,
            pid,
            address as usize + bytes.len(),
            0,
        )?;
        let raw = value.to_ne_bytes();
        bytes.extend_from_slice(&raw[..word.min(length - bytes.len())]);
    }
    Ok(bytes)
}

fn read_string(pid: pid_t, address: u64, budget: &mut usize) -> Result<OsString> {
    let mut bytes = Vec::new();
    // Read at aligned boundaries so a valid string at a page boundary does not
    // accidentally require access to the following page.
    let mut cursor = address;
    while *budget > 0 {
        let aligned = cursor & !7;
        let raw = ptrace(libc::PTRACE_PEEKDATA, pid, aligned as usize, 0)?.to_ne_bytes();
        for byte in &raw[(cursor - aligned) as usize..] {
            *budget -= 1;
            if *byte == 0 {
                return Ok(OsString::from_vec(bytes));
            }
            bytes.push(*byte);
            cursor += 1;
            if *budget == 0 {
                break;
            }
        }
    }
    Err(Error::new(
        ErrorKind::InvalidData,
        "exec strings exceed 2 MiB",
    ))
}

fn read_vector(pid: pid_t, address: u64, budget: &mut usize) -> Result<Vec<OsString>> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for index in 0..MAX_STRINGS {
        let pointer = u64::from_ne_bytes(
            read_memory(pid, address + index as u64 * 8, 8)?
                .try_into()
                .unwrap(),
        );
        if pointer == 0 {
            return Ok(values);
        }
        values.push(read_string(pid, pointer, budget)?);
    }
    Err(Error::new(
        ErrorKind::InvalidData,
        "too many exec arguments",
    ))
}

fn decode(pid: pid_t, parent_pid: Option<pid_t>, registers: &arch::Registers) -> Result<Exec> {
    let args = arch::args(registers);
    let at = arch::number(registers) == libc::SYS_execveat;
    let path = usize::from(at);
    let mut budget = MAX_BYTES;
    Ok(Exec {
        pid,
        parent_pid,
        executable: read_string(pid, args[path], &mut budget)?,
        argv: read_vector(pid, args[path + 1], &mut budget)?,
        environment: read_vector(pid, args[path + 2], &mut budget)?,
        cwd: std::fs::read_link(format!("/proc/{pid}/cwd"))?,
        dir_fd: at.then_some(args[0] as i32),
        flags: if at { args[4] } else { 0 },
    })
}

fn validate_replacement(replacement: &Replacement) -> Result<()> {
    if !replacement.executable.is_absolute() || replacement.argv.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "replacement requires an absolute executable and argv[0]",
        ));
    }
    c_string(replacement.executable.as_os_str())?;
    Ok(())
}

fn replacement_size(replacement: &Replacement, inherited: &[OsString]) -> Result<usize> {
    let environment = replacement.environment.as_deref().unwrap_or(inherited);
    let size = replacement.executable.as_os_str().len()
        + 1
        + replacement
            .argv
            .iter()
            .chain(environment)
            .map(|value| value.len() + 1)
            .sum::<usize>()
        + (replacement.argv.len() + environment.len() + 2) * 8
        + 16;
    if size > MAX_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "replacement exceeds 2 MiB",
        ));
    }
    Ok((size + 4095) & !4095)
}

fn layout(
    base: u64,
    replacement: &Replacement,
    inherited: &[OsString],
) -> Result<(Vec<u8>, u64, u64)> {
    let mut bytes = c_string(replacement.executable.as_os_str())?.into_bytes_with_nul();
    let mut vectors = Vec::new();
    for values in [
        &replacement.argv[..],
        replacement.environment.as_deref().unwrap_or(inherited),
    ] {
        let mut pointers = Vec::new();
        for value in values {
            pointers.push(base + bytes.len() as u64);
            bytes.extend(c_string(value)?.into_bytes_with_nul());
        }
        pointers.push(0);
        vectors.push(pointers);
    }
    bytes.resize((bytes.len() + 7) & !7, 0);
    let argv = base + bytes.len() as u64;
    for pointer in &vectors[0] {
        bytes.extend_from_slice(&pointer.to_ne_bytes());
    }
    let envp = base + bytes.len() as u64;
    for pointer in &vectors[1] {
        bytes.extend_from_slice(&pointer.to_ne_bytes());
    }
    Ok((bytes, argv, envp))
}

fn write_memory(pid: pid_t, address: u64, bytes: &[u8]) -> Result<()> {
    for (index, chunk) in bytes.chunks(8).enumerate() {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        ptrace(
            libc::PTRACE_POKEDATA,
            pid,
            address as usize + index * 8,
            u64::from_ne_bytes(word) as usize,
        )?;
    }
    Ok(())
}
