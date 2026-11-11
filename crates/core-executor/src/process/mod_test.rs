use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;

use super::*;

fn spec(argv: &[&str]) -> Spec {
  Spec {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    ..Spec::default()
  }
}

/// Every output event of `proc`, replayed, until its exit.
async fn collect(proc: &Proc) -> (Vec<u8>, Vec<u8>, Exit) {
  let Subscription { past, mut live, .. } = proc.subscribe(true, Subscriber::Owner);
  let mut stdout = Vec::new();
  let mut stderr = Vec::new();
  let mut events = past.into_iter().collect::<VecDeque<_>>();
  loop {
    let event = match events.pop_front() {
      Some(event) => event,
      None => live.recv().await.expect("the process exits"),
    };
    match event {
      Output::Stdout(b) => stdout.extend_from_slice(&b),
      Output::Stderr(b) => stderr.extend_from_slice(&b),
      Output::Exit(exit) => return (stdout, stderr, exit),
    }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn pipes_keep_stdout_and_stderr_apart_and_return_the_exit_code() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sh", "-c", "echo out; echo err >&2; exit 3"])).unwrap();

  let (stdout, stderr, exit) = collect(&proc).await;

  assert_eq!(stdout, b"out\n");
  assert_eq!(stderr, b"err\n");
  assert_eq!(
    exit,
    Exit {
      code: Some(3),
      signal: String::new()
    }
  );
  assert!(!proc.tty);
}

#[cfg(unix)]
#[tokio::test]
async fn a_client_attaching_after_the_exit_gets_the_output_replayed() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sh", "-c", "echo one; echo two"])).unwrap();
  proc.wait().await;

  let attached = host.get(&proc.id).expect("a finished process can be attached");
  let (stdout, _, exit) = collect(&attached).await;

  assert_eq!(stdout, b"one\ntwo\n");
  assert_eq!(exit.code, Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_reaches_the_process_and_eof_closes_it() {
  let host = Host::default();
  let proc = host.spawn(spec(&["cat"])).unwrap();

  proc.send(Input::Stdin(b"hello\n".to_vec()));
  proc.send(Input::Eof);
  let (stdout, _, exit) = collect(&proc).await;

  assert_eq!(stdout, b"hello\n");
  assert_eq!(exit.code, Some(0));
}

#[cfg(unix)]
#[tokio::test]
async fn a_signal_by_name_ends_the_process_with_that_signal() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sleep", "30"])).unwrap();

  proc.send(Input::Signal("SIGTERM".into()));
  let exit = proc.wait().await;

  assert_eq!(
    exit,
    Exit {
      code: None,
      signal: "SIGTERM".into()
    }
  );
}

// ROOT CAUSE:
//
// If the executor itself had SIGINT ignored (a daemon started with `&` from a
// non-interactive shell), a piped child ignored SIGINT too because POSIX keeps
// `SIG_IGN` across `exec`.
//
// Before the fix, `needy kill -s INT` left a piped `sleep` running.
// The fix resets the signal dispositions and the mask of a piped child before
// `exec`, as portable-pty does for a terminal's child.
#[cfg(unix)]
#[tokio::test]
async fn a_piped_child_dies_of_sigint_although_the_host_ignores_it() {
  use nix::sys::signal::{SigHandler, Signal, signal};
  // SAFETY: installs `SIG_IGN`, no handler code runs. It is process-wide, so
  // the previous disposition is restored before the test asserts.
  let previous = unsafe { signal(Signal::SIGINT, SigHandler::SigIgn) }.unwrap();
  let host = Host::default();
  let spawned = host.spawn(spec(&["sleep", "30"]));
  // SAFETY: restores the disposition saved above.
  unsafe { signal(Signal::SIGINT, previous) }.unwrap();
  let proc = spawned.unwrap();

  proc.send(Input::Signal("SIGINT".into()));
  let exit = tokio::time::timeout(Duration::from_secs(5), proc.wait()).await;
  if exit.is_err() {
    proc.send(Input::Signal("SIGKILL".into()));
  }

  assert_eq!(
    exit.expect("the child ends on SIGINT"),
    Exit {
      code: None,
      signal: "SIGINT".into()
    }
  );
}

#[cfg(unix)]
#[tokio::test]
async fn a_terminal_process_sees_a_terminal_and_its_signal_death() {
  let host = Host::default();
  let mut terminal = spec(&[
    "sh",
    "-c",
    "test -t 0 && test -t 1 && echo tty; kill -INT $$",
  ]);
  terminal.tty = Some((24, 80));
  let proc = host.spawn(terminal).unwrap();

  let (stdout, _, exit) = collect(&proc).await;

  assert!(proc.tty);
  assert!(String::from_utf8_lossy(&stdout).contains("tty"), "{stdout:?}");
  assert_eq!(exit.signal, "SIGINT");
}

#[tokio::test]
async fn a_missing_program_cannot_start() {
  let host = Host::default();

  let error = host.spawn(spec(&["/nonexistent/needy-test-command"])).err().expect("the program is missing");

  assert!(matches!(error, Error::Start { .. }), "{error:?}");
  assert!(error.to_string().starts_with("cannot start /nonexistent/needy-test-command"), "{error}");
  assert!(host.list().is_empty());
}

fn exit(code: Option<i32>, signal: &str) -> Exit {
  Exit {
    code,
    signal: signal.into(),
  }
}

// ROOT CAUSE:
//
// If needy ended a Windows process for a signal (`needy kill`), the process
// was listed as `exited 1`, because Windows has no signals: `taskkill /F`
// terminates it with exit code 1, which reads like the program's own
// failure.
//
// Before the fix, `needy ps` showed state `exited`, EXIT `1`.
// The fix reports the signal needy delivered, as on Unix: state `killed`.
#[test]
fn a_process_needy_terminated_where_signals_terminate_reports_the_signal() {
  assert_eq!(proc::ended(exit(Some(1), ""), Some("SIGTERM"), true), exit(None, "SIGTERM"));
}

#[test]
fn a_terminated_exit_is_kept_as_it_is_where_signals_are_delivered() {
  assert_eq!(proc::ended(exit(None, "SIGTERM"), Some("SIGTERM"), false), exit(None, "SIGTERM"));
  assert_eq!(proc::ended(exit(Some(1), ""), Some("SIGTERM"), false), exit(Some(1), ""));
}

#[test]
fn an_exit_other_than_terminations_is_the_programs_own() {
  // The program exited on its own before the termination took effect.
  assert_eq!(proc::ended(exit(Some(0), ""), Some("SIGTERM"), true), exit(Some(0), ""));
  assert_eq!(proc::ended(exit(Some(1), ""), None, true), exit(Some(1), ""));
}

// ROOT CAUSE:
//
// If the working directory of a command did not exist on the executor, the
// spawn failed with ENOENT, which read like a missing program:
// `cannot start bash: No such file or directory`.
//
// Before the fix, the host reported the `chdir` failure as the program's.
// The fix checks the directory first and names it in the error.
#[tokio::test]
async fn a_missing_working_directory_is_named_instead_of_the_program() {
  let host = Host::default();
  let dir = tempfile::tempdir().unwrap();
  let missing = dir.path().join("needy-missing-dir");
  let mut command = spec(&["sh", "-c", "true"]);
  command.cwd = missing.display().to_string();

  let error = host.spawn(command).err().expect("the directory is missing");

  assert!(matches!(error, Error::WorkingDirectory { .. }), "{error:?}");
  assert_eq!(error.to_string(), format!("cannot start sh: working directory {} does not exist", missing.display()));
  assert!(host.list().is_empty());
}

#[tokio::test]
async fn a_working_directory_that_is_a_file_is_named_on_a_terminal_too() {
  let host = Host::default();
  let file = tempfile::NamedTempFile::new().unwrap();
  let mut command = spec(&["sh", "-c", "true"]);
  command.cwd = file.path().display().to_string();
  command.tty = Some((24, 80));

  let error = host.spawn(command).err().expect("the directory is a file");

  assert_eq!(error.to_string(), format!("cannot start sh: working directory {} is not a directory", file.path().display()));
}

#[cfg(unix)]
#[tokio::test]
async fn a_finished_process_is_listed_until_its_retention_runs_out() {
  let host = Host::new(Duration::from_millis(300));
  let finished = host.spawn(spec(&["true"])).unwrap();
  let running = host.spawn(spec(&["sleep", "30"])).unwrap();
  finished.wait().await;

  let listed: BTreeSet<_> = host.list().iter().map(|p| p.id.clone()).collect();
  assert_eq!(listed, BTreeSet::from([finished.id.clone(), running.id.clone()]), "a process that just finished is still listed");

  tokio::time::sleep(Duration::from_millis(400)).await;
  let listed: BTreeSet<_> = host.list().iter().map(|p| p.id.clone()).collect();
  assert_eq!(listed, BTreeSet::from([running.id.clone()]), "the finished process expired; the running one stays");
  assert!(host.get(&finished.id).is_none());

  running.send(Input::Signal("SIGKILL".into()));
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_kills_running_processes_and_refuses_new_ones() {
  let host = Host::default();
  let proc = host.spawn(spec(&["sleep", "30"])).unwrap();

  tokio::time::timeout(Duration::from_secs(10), host.shutdown()).await.expect("shutdown waits for the host's tasks");

  assert_eq!(proc.exit().map(|e| e.signal), Some("SIGKILL".into()));
  assert!(matches!(host.spawn(spec(&["true"])), Err(Error::ShuttingDown)));
}

// ROOT CAUSE:
//
// If one client attached to a process and stopped reading, the process and
// every other client stalled, because output fan-out awaited each
// subscriber in turn while it held the subscribers' lock.
//
// Before the fix, the client that started the process stopped receiving
// output once the idle client's queue was full.
// The fix lets only the starting client hold the process back and drops a
// viewer that cannot keep up, saying so.
//
// ROOT CAUSE (of this test's flakiness):
//
// If the test's real `sh` and its pipes were slow or chunked its output
// differently, the test failed although the fan-out was right, because it
// judged the fan-out through things it did not control: how many output
// events the pipe reader produced (3000 free lines read in fewer chunks than
// the viewer's queue holds, fixed in `903070f` by pacing one line per event),
// and then still the OS scheduler, the shell writing each `echo` in one
// `write`, the exit being reaped, and a 20 s wall-clock bound around 200
// paced round trips through a child process. The paced version did not fail
// again in stress runs on macOS and Linux, so which of these failed on a Mac
// is not known; the fan-out itself is not one of them.
//
// Before the fix, whether the viewer fell behind and whether the owner got
// everything in time depended on that process.
// The fix feeds the process's output to the fan-out directly, one event per
// `emit`, as the pipe reader does, and checks that each `emit` completes
// without waiting: a viewer that stopped reading makes it wait, so the
// regression fails at the event that blocks, with no clock and no process.
#[tokio::test]
async fn a_viewer_that_stops_reading_holds_back_neither_the_process_nor_its_owner() {
  use futures_util::FutureExt;
  // NOTICE: more events than a subscriber's queue holds (64,
  // `SUBSCRIBER_QUEUE` in `proc.rs`).
  const EVENTS: usize = 200;
  let (proc, ends) = Proc::new(&spec(&["writer"]), -1, false, None);
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let idle = proc.subscribe(true, Subscriber::Viewer);

  // NOTICE: every `emit` here runs inside one poll of this task, and each
  // takes two operations of tokio's cooperative budget (128 per poll, the
  // order lock and the owner's `send`), so a budgeted `emit` would yield at
  // event 64 although nothing holds it back. `unconstrained` lifts the budget.
  // Source: https://github.com/tokio-rs/tokio/blob/tokio-1.54.1/tokio/src/task/coop/mod.rs#L115-L117
  // Reference: https://docs.rs/tokio/1.54.1/tokio/task/coop/fn.unconstrained.html
  for i in 0..EVENTS {
    let line = format!("line {i}\n");
    tokio::task::coop::unconstrained(proc.emit(Output::Stdout(Bytes::from(line.clone()))))
      .now_or_never()
      .unwrap_or_else(|| panic!("a viewer that stopped reading held the process back at event {i}"));
    match owner.live.try_recv() {
      Ok(Output::Stdout(bytes)) => assert_eq!(bytes, line.as_bytes(), "the owner's event {i}"),
      other => panic!("the owner did not get event {i}: {other:?}"),
    }
  }
  let exit = Exit {
    code: Some(0),
    signal: String::new(),
  };
  tokio::task::coop::unconstrained(proc.finish(&ends.exit, exit.clone()))
    .now_or_never()
    .expect("a viewer that stopped reading held the exit back");

  match owner.live.try_recv() {
    Ok(Output::Exit(got)) => assert_eq!(got, exit),
    other => panic!("the owner did not get the exit: {other:?}"),
  }
  assert!(idle.fell_behind());
}

/// Reads `live` until its text contains `marker`, and returns all of it.
async fn read_until(live: &mut Live, marker: &str) -> String {
  let mut text = String::new();
  while !text.contains(marker) {
    match live.recv().await {
      Some(Output::Stdout(bytes)) => text.push_str(&String::from_utf8_lossy(&bytes)),
      Some(_) => {}
      None => break,
    }
  }
  text
}

// ROOT CAUSE:
//
// If the client's input ended while a program had its terminal in raw
// mode, the program read a Ctrl-D keystroke, because end of input on a
// terminal was always written as `0x04`.
//
// Before the fix, the raw-mode reader below printed `04`.
// The fix writes the end-of-file character only when the terminal reads by
// lines (ICANON); otherwise input just stops.
#[cfg(unix)]
#[tokio::test]
async fn end_of_input_reaches_a_raw_mode_program_as_nothing() {
  let host = Host::default();
  let mut raw = spec(&[
    "sh",
    "-c",
    "stty raw -echo; echo ready; dd bs=1 count=1 2>/dev/null | od -An -tx1; echo read",
  ]);
  raw.tty = Some((24, 80));
  let proc = host.spawn(raw).unwrap();
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let mut text: String = owner
    .past
    .iter()
    .filter_map(|o| {
      if let Output::Stdout(b) = o {
        Some(String::from_utf8_lossy(b).into_owned())
      } else {
        None
      }
    })
    .collect();
  if !text.contains("ready") {
    text.push_str(&read_until(&mut owner.live, "ready").await);
  }

  proc.send(Input::Eof);
  let after = tokio::time::timeout(Duration::from_millis(700), read_until(&mut owner.live, "read")).await.unwrap_or_default();
  proc.send(Input::Signal("SIGKILL".into()));

  assert!(!after.contains("04"), "the program read a Ctrl-D: {after:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn end_of_input_ends_a_line_reading_program_on_a_terminal() {
  let host = Host::default();
  let mut cat = spec(&["cat"]);
  cat.tty = Some((24, 80));
  let proc = host.spawn(cat).unwrap();

  proc.send(Input::Eof);
  let exit = tokio::time::timeout(Duration::from_secs(10), proc.wait()).await.expect("cat did not see the end of input");

  assert_eq!(exit.code, Some(0));
}

/// The `TERM` a terminal process with `term` in its environment sees, and
/// what the process records about it.
#[cfg(unix)]
async fn term_seen(term: &str) -> (String, Option<TermFallback>) {
  let host = Host::default();
  let mut probe = spec(&["sh", "-c", "echo \"[$TERM]\""]);
  probe.tty = Some((24, 80));
  probe.env = vec![("TERM".into(), term.into())];
  let proc = host.spawn(probe).unwrap();
  let (stdout, _, _) = collect(&proc).await;
  (String::from_utf8_lossy(&stdout).trim().to_string(), proc.term_fallback.clone())
}

// ROOT CAUSE:
//
// If the client's terminal type had no terminfo entry on the executor, a
// program there could not draw (`tput: unknown terminal`), because the
// client's `TERM` was passed on as it was.
//
// Before the fix, the process saw the unknown `TERM`.
// The fix falls back to `xterm-256color` and records the fallback.
#[cfg(unix)]
#[tokio::test]
async fn an_unknown_terminal_type_falls_back_to_xterm_256color() {
  let (seen, fallback) = term_seen("needy-no-such-terminal").await;

  assert_eq!(seen, "[xterm-256color]");
  assert_eq!(
    fallback,
    Some(TermFallback {
      requested: "needy-no-such-terminal".into(),
      used: "xterm-256color".into()
    })
  );
}

#[cfg(unix)]
#[tokio::test]
async fn a_known_terminal_type_is_kept() {
  let (seen, fallback) = term_seen("vt100").await;

  assert_eq!(seen, "[vt100]");
  assert_eq!(fallback, None);
}

// ROOT CAUSE:
//
// If a process on a terminal left a descendant in its own session, such as
// `setsid sleep 600 &`, its exit was not reported until the descendant
// ended, because the descendant kept the terminal open and the output
// reader, which the exit waits for, blocked in `read` on Linux.
//
// Before the fix, `exec -t -- sh -c 'setsid sleep 600 & exit 3'` hung on
// Linux (the terminal note's G8); macOS ended the read at the exit.
// The fix stops the reader after the drain period that follows the exit.
#[cfg(unix)]
#[tokio::test]
async fn a_terminal_process_exits_while_a_descendant_in_its_own_session_keeps_the_terminal() {
  let dir = tempfile::tempdir().unwrap();
  let pid_file = dir.path().join("descendant.pid");
  let script = dir.path().join("leave-a-descendant.sh");
  // `setsid(1)` is missing on macOS; perl's POSIX module has setsid(2).
  std::fs::write(
    &script,
    format!("perl -MPOSIX -e 'setsid(); open(my $f, \">\", \"{}\"); print $f $$; close $f; exec \"sleep\", \"600\"' &\nwhile [ ! -s {} ]; do sleep 0.05; done\nexit 3\n", pid_file.display(), pid_file.display()),
  )
  .unwrap();
  let host = Host::default();
  let mut leaves = spec(&["sh", script.to_str().unwrap()]);
  leaves.tty = Some((24, 80));
  let proc = host.spawn(leaves).unwrap();

  let exit = tokio::time::timeout(Duration::from_secs(5), collect(&proc)).await;
  let descendant: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
  // The descendant is this test's to end; one that is gone already is fine.
  let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(descendant), nix::sys::signal::Signal::SIGKILL);

  let (_, _, exit) = exit.expect("the exit was not reported while the descendant held the terminal");
  assert_eq!(exit.code, Some(3));
}

/// A terminal of `rows` by `cols` that shows what a client receives.
fn terminal(rows: u16, cols: u16) -> vt100::Parser {
  vt100::Parser::new(std::num::NonZeroU16::new(rows).unwrap(), std::num::NonZeroU16::new(cols).unwrap(), 0)
}

/// Feeds `live` into `screen` until its contents include `marker`.
async fn render_until(live: &mut Live, screen: &mut vt100::Parser, marker: &str) {
  while !screen.screen().contents().contains(marker) {
    match live.recv().await {
      Some(Output::Stdout(bytes)) => screen.process(&bytes),
      Some(_) => {}
      None => panic!("the subscription ended before {marker:?} was drawn"),
    }
  }
}

/// A program that enters the alternate screen, then draws more than the
/// replay buffer holds with cursor moves, and waits.
const FULL_SCREEN: &str = r#"printf '\033[?1049h\033[2J\033[1;1HTITLE\033[?2004h'
yes "$(printf '\033[5;1Hfiller')" | head -c 400000
printf '\033[5;1Hframe drawn\033[3;7H'
sleep 30"#;

// ROOT CAUSE:
//
// If a client attached to a full-screen program after more output than the
// replay buffer holds, it got the buffer's raw bytes, which started after
// the program entered the alternate screen.
//
// Before the fix, the client drew the frame on its normal screen, without
// the program's modes.
// The fix draws the screen from a terminal-state model instead.
#[cfg(unix)]
#[tokio::test]
async fn a_client_attaching_to_a_full_screen_program_sees_the_programs_screen() {
  let host = Host::default();
  let mut tty = spec(&["sh", "-c", FULL_SCREEN]);
  tty.tty = Some((12, 50));
  let proc = host.spawn(tty).unwrap();
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let mut program = terminal(12, 50);
  for event in std::mem::take(&mut owner.past) {
    if let Output::Stdout(bytes) = event {
      program.process(&bytes);
    }
  }
  tokio::time::timeout(Duration::from_secs(20), render_until(&mut owner.live, &mut program, "frame drawn")).await.unwrap();

  let attached = proc.subscribe(true, Subscriber::Viewer);
  let mut client = terminal(12, 50);
  client.process(b"the client's own prompt$ ");
  for event in attached.past {
    if let Output::Stdout(bytes) = event {
      client.process(&bytes);
    }
  }

  assert_eq!(client.screen().contents(), program.screen().contents());
  assert!(client.screen().alternate_screen());
  assert!(client.screen().bracketed_paste());
  assert_eq!(client.screen().cursor_position(), program.screen().cursor_position());
  proc.send(Input::Signal("SIGKILL".into()));
}

// A terminal viewer that falls behind skips output and is drawn the screen
// again once it reads, instead of being dropped (G5, then G1). Here the
// redraw comes with the next output, which finds room in the viewer's queue
// before the viewer has read it all; the quiet program's case is
// `a_lagging_viewer_is_redrawn_when_it_catches_up_although_the_program_stays_quiet`.
//
// NOTICE: the root cause below describes the fan-out before a viewer got
// its pending redraw without new output; it is kept as the record of the
// flaky test.
//
// ROOT CAUSE (of this test's flakiness):
//
// If the test task drained the viewer's queue more than a second after the
// owner read "burst done", the test timed out at "final", because the test
// ran a real `sh` paced by `sleep 1`: the viewer's queue was still full
// when "final" came, so the viewer skipped it as well, and a stale viewer
// is drawn again only at the next output, which a program that then sleeps
// 30 s never writes (the NOTICE in `Proc::emit`). Under load the owner's
// render of the 400 kB burst alone could take that second.
//
// Before the fix, the test failed under load on a Linux KVM host (78 of
// 300 runs, and 47 of 300 with every core busy, each at "final") although
// the fan-out did what it is meant to.
// The fix feeds the output to the fan-out directly, one event per `emit`,
// and drains the viewer at fixed points between them, so the burst, the
// drain and the next output come in one order with no process and no
// clock.
#[tokio::test]
async fn a_terminal_viewer_that_falls_behind_is_redrawn_not_dropped() {
  use futures_util::FutureExt;
  // NOTICE: more events than a subscriber's queue holds (64,
  // `SUBSCRIBER_QUEUE` in `proc.rs`).
  const BURST: usize = 200;
  let mut tty = spec(&["writer"]);
  tty.tty = Some((6, 30));
  let (proc, _ends) = Proc::new(&tty, -1, true, None);
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let mut viewer = proc.subscribe(true, Subscriber::Viewer);
  let mut program = terminal(6, 30);
  // Writes `text` as the program would, then draws what the owner got.
  // NOTICE: `unconstrained`, as in the stalled-viewer test above: a
  // budgeted `emit` would yield after 64 events in one poll although
  // nothing holds it back.
  let mut write = |text: &str| {
    tokio::task::coop::unconstrained(proc.emit(Output::Stdout(Bytes::from(text.to_string()))))
      .now_or_never()
      .unwrap_or_else(|| panic!("the viewer held the program back at {text:?}"));
    match owner.live.try_recv() {
      Ok(Output::Stdout(bytes)) => program.process(&bytes),
      other => panic!("the owner did not get {text:?}: {other:?}"),
    }
  };
  // What the viewer has now, up to `limit` events, drawn on its terminal.
  let mut client = terminal(6, 30);
  let mut read = |client: &mut vt100::Parser, limit: usize| {
    for _ in 0..limit {
      match viewer.live.try_recv() {
        Ok(Output::Stdout(bytes)) => client.process(&bytes),
        Ok(other) => panic!("the viewer got {other:?}"),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => panic!("the viewer was dropped"),
      }
    }
  };

  for i in 0..BURST {
    write(&format!("\x1b[2;1Hburst {i}"));
  }
  write("\x1b[3;1Hburst done");
  // The viewer reads one event, which makes room in its queue, so the next
  // output finds room for the pending redraw before the queue is empty.
  read(&mut client, 1);
  // The viewer skipped output it had no room for.
  assert!(!client.screen().contents().contains("burst done"), "{}", client.screen().contents());
  write("\x1b[4;1Hfinal");
  read(&mut client, usize::MAX);

  assert!(!viewer.fell_behind());
  assert_eq!(client.screen().contents(), program.screen().contents());
}

/// A terminal process with no process behind it, whose output a test feeds
/// to the fan-out itself.
fn terminal_proc(rows: u16, cols: u16) -> (Arc<Proc>, proc::Ends) {
  let mut tty = spec(&["writer"]);
  tty.tty = Some((rows, cols));
  Proc::new(&tty, -1, true, None)
}

/// Writes `text` as the program would, through the fan-out, and fails if
/// anything holds the program back.
fn emit_now(proc: &Proc, text: &str) {
  use futures_util::FutureExt;
  // NOTICE: `unconstrained`, as in the stalled-viewer test: a budgeted
  // `emit` would yield after 64 events in one poll although nothing holds
  // it back.
  tokio::task::coop::unconstrained(proc.emit(Output::Stdout(Bytes::from(text.to_string()))))
    .now_or_never()
    .unwrap_or_else(|| panic!("a subscriber held the program back at {text:?}"));
}

/// The next output of `subscription` that `recv`, as the relay calls it,
/// gives without waiting; `None` when it would wait.
fn next_now(subscription: &mut Subscription) -> Option<Bytes> {
  use futures_util::FutureExt;
  // NOTICE: `unconstrained`: a `recv` that runs out of tokio's cooperative
  // budget (128 per poll) is pending although an event is queued.
  match tokio::task::coop::unconstrained(subscription.live.recv()).now_or_never() {
    None => None,
    Some(Some(Output::Stdout(bytes))) => Some(bytes),
    Some(other) => panic!("the subscriber got {other:?}"),
  }
}

// ROOT CAUSE:
//
// If a terminal viewer fell behind during a burst and the program then
// wrote nothing more (a shell back at its prompt), the viewer kept a stale,
// partial screen after it had read everything queued, because a viewer that
// skipped output was drawn again only at the program's next output.
//
// Before the fix, the viewer's `recv` waited with nothing to give once its
// queue was empty, and its terminal showed the screen of the last event it
// had room for.
// The fix gives a viewer that skipped output one pending redraw, drawn from
// the model as soon as its queue is empty, whether or not the program writes
// again, as herdr's client writer asks for a full frame once it takes the
// frame from its one-slot render lane (the terminal note's G5).
#[tokio::test]
async fn a_lagging_viewer_is_redrawn_when_it_catches_up_although_the_program_stays_quiet() {
  // NOTICE: more events than a subscriber's queue holds (64,
  // `SUBSCRIBER_QUEUE` in `proc.rs`).
  const BURST: usize = 200;
  let (proc, _ends) = terminal_proc(6, 30);
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let mut reading = proc.subscribe(true, Subscriber::Viewer);
  let mut lagging = proc.subscribe(true, Subscriber::Viewer);
  let mut program = terminal(6, 30);
  let mut beside = terminal(6, 30);

  for i in 0..BURST {
    let text = format!("\x1b[2;1Hburst {i}\x1b[{};1Hrow {i}", 3 + i % 3);
    emit_now(&proc, &text);
    // The starting client and a viewer that keeps up get every event, in
    // order, while the lagging viewer reads nothing.
    match owner.live.try_recv() {
      Ok(Output::Stdout(bytes)) => {
        assert_eq!(bytes, text.as_bytes(), "the owner's event {i}");
        program.process(&bytes);
      }
      other => panic!("the owner did not get event {i}: {other:?}"),
    }
    let bytes = next_now(&mut reading).unwrap_or_else(|| panic!("the reading viewer did not get event {i}"));
    assert_eq!(bytes, text.as_bytes(), "the reading viewer's event {i}");
    beside.process(&bytes);
  }

  // The program stays quiet; the lagging viewer reads what it has.
  let mut client = terminal(6, 30);
  let mut received = 0;
  while let Some(bytes) = next_now(&mut lagging) {
    client.process(&bytes);
    received += 1;
    assert!(received <= BURST, "the lagging viewer kept receiving");
  }

  assert!(received < BURST, "the viewer did not fall behind: it got {received} events");
  assert!(!lagging.fell_behind());
  assert_eq!(client.screen().contents(), program.screen().contents());
  assert_eq!(client.screen().cursor_position(), program.screen().cursor_position());
  assert_eq!(beside.screen().contents(), program.screen().contents());
  // One redraw: after it the viewer waits for output again, and gets it.
  assert!(next_now(&mut lagging).is_none());
  emit_now(&proc, "\x1b[6;1Hafter");
  assert_eq!(next_now(&mut lagging).expect("live output follows the redraw"), "\x1b[6;1Hafter".as_bytes());
}

// A viewer that skipped output is drawn again even when the screen's text
// is what it already shows: the skipped output may have changed what the
// text does not show, here a mode. needy keeps no copy of what each viewer
// shows, so it never leaves out a redraw as identical; herdr, which does,
// had to force a frame where its buffers compared equal (commits
// `61f64006`, `c9fca9fb`).
#[tokio::test]
async fn a_lagging_viewer_is_redrawn_although_the_skipped_output_left_the_text_unchanged() {
  // NOTICE: more events than a subscriber's queue holds (64,
  // `SUBSCRIBER_QUEUE` in `proc.rs`).
  const BURST: usize = 200;
  let (proc, _ends) = terminal_proc(6, 30);
  let mut lagging = proc.subscribe(true, Subscriber::Viewer);
  let mut program = terminal(6, 30);
  for _ in 0..BURST {
    emit_now(&proc, "\x1b[1;1Hsame");
    program.process(b"\x1b[1;1Hsame");
  }
  emit_now(&proc, "\x1b[?2004h");
  program.process(b"\x1b[?2004h");

  let mut client = terminal(6, 30);
  loop {
    match lagging.live.try_recv() {
      Ok(Output::Stdout(bytes)) => client.process(&bytes),
      Ok(other) => panic!("the viewer got {other:?}"),
      Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
      Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => panic!("the viewer was dropped"),
    }
  }

  assert_eq!(client.screen().contents(), program.screen().contents());
  assert!(client.screen().bracketed_paste(), "the viewer missed the mode the skipped output turned on");
}
