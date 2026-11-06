use super::{Result, ptrace};
use libc::{c_void, iovec, pid_t, user_regs_struct};

pub type Registers = user_regs_struct;

pub fn read(pid: pid_t) -> Result<Registers> {
    let mut registers = unsafe { std::mem::zeroed::<Registers>() };
    let mut vector = iovec {
        iov_base: (&mut registers as *mut Registers).cast(),
        iov_len: std::mem::size_of::<Registers>(),
    };
    ptrace(
        libc::PTRACE_GETREGSET,
        pid,
        1,
        (&mut vector as *mut iovec) as usize,
    )?;
    Ok(registers)
}

pub fn write(pid: pid_t, registers: &Registers) -> Result<()> {
    let mut vector = iovec {
        iov_base: (registers as *const Registers).cast_mut().cast::<c_void>(),
        iov_len: std::mem::size_of::<Registers>(),
    };
    ptrace(
        libc::PTRACE_SETREGSET,
        pid,
        1,
        (&mut vector as *mut iovec) as usize,
    )?;
    Ok(())
}

#[cfg(target_arch = "x86_64")]
pub const AUDIT_ARCH: u32 = 0xc000003e;
#[cfg(target_arch = "aarch64")]
pub const AUDIT_ARCH: u32 = 0xc00000b7;

#[cfg(target_arch = "x86_64")]
pub fn number(regs: &Registers) -> i64 {
    regs.orig_rax as i64
}
#[cfg(target_arch = "aarch64")]
pub fn number(regs: &Registers) -> i64 {
    regs.regs[8] as i64
}

#[cfg(target_arch = "x86_64")]
pub fn args(r: &Registers) -> [u64; 6] {
    [r.rdi, r.rsi, r.rdx, r.r10, r.r8, r.r9]
}
#[cfg(target_arch = "aarch64")]
pub fn args(r: &Registers) -> [u64; 6] {
    r.regs[..6].try_into().unwrap()
}

#[cfg(target_arch = "x86_64")]
pub fn result(r: &Registers) -> i64 {
    r.rax as i64
}
#[cfg(target_arch = "aarch64")]
pub fn result(r: &Registers) -> i64 {
    r.regs[0] as i64
}

#[cfg(target_arch = "x86_64")]
pub fn set_result(r: &mut Registers, value: i64) {
    r.rax = value as u64;
}
#[cfg(target_arch = "aarch64")]
pub fn set_result(r: &mut Registers, value: i64) {
    r.regs[0] = value as u64;
}

#[cfg(target_arch = "x86_64")]
pub fn set_call(r: &mut Registers, nr: i64, a: [u64; 6]) {
    r.orig_rax = nr as u64;
    r.rdi = a[0];
    r.rsi = a[1];
    r.rdx = a[2];
    r.r10 = a[3];
    r.r8 = a[4];
    r.r9 = a[5];
}
#[cfg(target_arch = "aarch64")]
pub fn set_call(r: &mut Registers, nr: i64, a: [u64; 6]) {
    r.regs[8] = nr as u64;
    r.regs[..6].copy_from_slice(&a);
}

pub fn replace_pending(pid: pid_t, r: &mut Registers, nr: i64, a: [u64; 6]) -> Result<()> {
    set_call(r, nr, a);
    write(pid, r)?;
    #[cfg(target_arch = "aarch64")]
    {
        // arm64 keeps the in-flight syscall number separately from x8.
        let mut number = nr as i32;
        let mut vector = iovec {
            iov_base: (&mut number as *mut i32).cast(),
            iov_len: 4,
        };
        ptrace(
            libc::PTRACE_SETREGSET,
            pid,
            0x404,
            (&mut vector as *mut iovec) as usize,
        )?;
    }
    Ok(())
}

pub fn restart_call(r: &mut Registers, number: i64, arguments: [u64; 6]) {
    set_call(r, number, arguments);
    #[cfg(target_arch = "x86_64")]
    {
        r.rip -= 2;
        r.rax = number as u64;
    }
    #[cfg(target_arch = "aarch64")]
    {
        r.pc -= 4;
    }
}

pub fn restart_exec(r: &mut Registers, pathname: u64, argv: u64, environment: u64) {
    restart_call(r, libc::SYS_execve, [pathname, argv, environment, 0, 0, 0]);
}
