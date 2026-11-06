#![cfg(target_os = "linux")]

use needy_exec_intercept::{Action, Command, Outcome, Replacement, supervise};
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

fn fixture(mode: &str) -> Command {
    if let Some(binary) = std::env::var_os("NEEDY_KERNEL_FIXTURE") {
        let mut command = Command::new(std::path::PathBuf::from(binary)).unwrap();
        command.args = vec![mode.into()];
        return command;
    }
    static FIXTURE: std::sync::OnceLock<(tempfile::TempDir, std::path::PathBuf)> =
        std::sync::OnceLock::new();
    let (_, binary) = FIXTURE.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("fixture.c");
        let binary = directory.path().join("fixture");
        std::fs::write(&source, include_str!("fixture.c")).unwrap();
        assert!(
            std::process::Command::new("cc")
                .args(["-pthread", "-O0", "-o"])
                .arg(&binary)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        (directory, binary)
    });
    let mut command = Command::new(binary).unwrap();
    command.args = vec![mode.into()];
    command
}

fn replacement(code: i32) -> Action {
    Action::Redirect(Replacement {
        executable: "/bin/sh".into(),
        argv: vec!["proxy".into(), "-c".into(), format!("exit {code}").into()],
        environment: None,
    })
}

#[test]
fn execveat_empty_path_redirects_without_trusting_argv0() {
    let report = supervise(fixture("execveat"), &AtomicBool::new(false), |exec| {
        assert!(exec.dir_fd.is_some());
        assert_eq!(exec.flags, libc::AT_EMPTY_PATH as u64);
        assert!(exec.executable.is_empty());
        assert_eq!(exec.argv[0], "untrusted-argv0");
        Ok(replacement(23))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(23));
    assert_eq!(report.redirected, 1);
}

#[test]
fn exec_from_nonleader_thread_transfers_trace_state() {
    let report = supervise(fixture("thread"), &AtomicBool::new(false), |_| {
        Ok(replacement(29))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(29));
    assert_eq!(report.redirected, 1);
}

#[test]
fn arbitrary_argument_and_environment_bytes_are_preserved() {
    let report = supervise(fixture("bytes"), &AtomicBool::new(false), |exec| {
        assert_eq!(exec.executable, "/usr/bin/uname");
        assert_eq!(exec.argv[1], "space argument");
        assert_eq!(exec.argv[2], "quote'\"");
        assert_eq!(exec.argv[3].as_bytes(), &[0xff]);
        assert_eq!(exec.environment[1].as_bytes(), b"BINARY=\xfe");
        Ok(replacement(31))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(31));
}

#[test]
fn invalid_exec_pointer_returns_native_efault() {
    let report = supervise(fixture("invalid"), &AtomicBool::new(false), |_| {
        panic!("invalid exec must reach kernel validation")
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(0));
}

#[test]
fn vfork_redirect_does_not_leak_scratch_mappings_into_parent() {
    let report = supervise(fixture("vfork"), &AtomicBool::new(false), |_| {
        Ok(replacement(0))
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(0));
    assert_eq!(report.redirected, 24);
}
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh").unwrap();
    command.args = vec!["-c".into(), script.into()];
    command
}

#[test]
fn root_first_exec_stays_local_and_returns_its_real_status() {
    let report = supervise(shell("exit 17"), &AtomicBool::new(false), |_| {
        panic!("the root's first exec must not be offered to placement")
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(17));
    assert_eq!(report.redirected, 0);
    assert_eq!(report.injection_stops, 0);
    assert_eq!(report.exec_events, 1);
}

#[test]
fn redirect_preserves_parent_wait_and_proxy_descendants_stay_local() {
    let report = supervise(
        shell("out=$(/usr/bin/uname); test \"$out\" = redirected"),
        &AtomicBool::new(false),
        |exec| {
            assert!(exec.pid > 0);
            assert!(exec.parent_pid.is_some());
            assert_eq!(exec.argv[0], "/usr/bin/uname");
            assert!(exec.cwd.is_absolute());
            assert!(
                exec.environment
                    .iter()
                    .any(|value| value.to_string_lossy().starts_with("PATH="))
            );
            Ok(Action::Redirect(Replacement {
                executable: "/bin/sh".into(),
                argv: vec![
                    "proxy".into(),
                    "-c".into(),
                    "/usr/bin/printf redirected".into(),
                ],
                environment: None,
            }))
        },
    )
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(0));
    assert_eq!(report.redirected, 1);
    assert_eq!(report.injection_stops, 1);
}

#[test]
fn denied_exec_returns_errno_to_the_original_program() {
    let report = supervise(
        shell("/usr/bin/uname >/dev/null 2>&1; test $? -eq 127"),
        &AtomicBool::new(false),
        |_| Ok(Action::Deny(libc::ENOENT)),
    )
    .unwrap();
    assert_eq!(report.outcome, Outcome::Exited(0));
    assert_eq!(report.redirected, 0);
    assert_eq!(report.injection_stops, 1);
}

#[test]
fn native_signal_exit_is_preserved() {
    let report = supervise(shell("kill -TERM $$"), &AtomicBool::new(false), |_| {
        Ok(Action::Continue)
    })
    .unwrap();
    assert_eq!(report.outcome, Outcome::Signaled(libc::SIGTERM));
}

#[test]
fn policy_error_kills_and_reaps_the_stopped_child() {
    let child = AtomicI32::new(0);
    let error = supervise(shell("/bin/sleep 60"), &AtomicBool::new(false), |exec| {
        child.store(exec.pid, Ordering::Relaxed);
        Err(std::io::Error::other("placement unavailable"))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "placement unavailable");
    assert!(child.load(Ordering::Relaxed) > 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while unsafe { libc::kill(child.load(Ordering::Relaxed), 0) } == 0
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        unsafe { libc::kill(child.load(Ordering::Relaxed), 0) },
        -1,
        "pid={} state={:?}",
        child.load(Ordering::Relaxed),
        std::fs::read_to_string(format!("/proc/{}/stat", child.load(Ordering::Relaxed)))
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn cancellation_terminates_the_supervised_tree() {
    let flag = Arc::new(AtomicBool::new(false));
    let cancel = flag.clone();
    let thread = std::thread::spawn(move || {
        supervise(shell("/bin/sleep 60"), &cancel, |_| Ok(Action::Continue))
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    flag.store(true, Ordering::Relaxed);
    assert_eq!(
        thread.join().unwrap().unwrap_err().kind(),
        std::io::ErrorKind::Interrupted
    );
}
