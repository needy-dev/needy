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
#[cfg(unix)]
#[tokio::test]
async fn a_viewer_that_stops_reading_holds_back_neither_the_process_nor_its_owner() {
  let host = Host::default();
  let proc = host
    .spawn(spec(&[
      "sh",
      "-c",
      "sleep 0.2; i=0; while [ $i -lt 3000 ]; do echo line $i; i=$((i+1)); done",
    ]))
    .unwrap();
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let idle = proc.subscribe(true, Subscriber::Viewer);

  let drained = async {
    while let Some(event) = owner.live.recv().await {
      if let Output::Exit(exit) = event {
        return exit;
      }
    }
    panic!("the owner's subscription closed before the exit");
  };
  let exit = tokio::time::timeout(Duration::from_secs(10), drained).await.expect("a viewer that stopped reading held the process back");

  assert_eq!(exit.code, Some(0));
  assert!(idle.fell_behind());
}

/// Reads `live` until its text contains `marker`, and returns all of it.
async fn read_until(live: &mut tokio::sync::mpsc::Receiver<Output>, marker: &str) -> String {
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
async fn render_until(live: &mut tokio::sync::mpsc::Receiver<Output>, screen: &mut vt100::Parser, marker: &str) {
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
// again once it reads, instead of being dropped (G5, then G1).
#[cfg(unix)]
#[tokio::test]
async fn a_terminal_viewer_that_falls_behind_is_redrawn_not_dropped() {
  let host = Host::default();
  let mut tty = spec(&[
    "sh",
    "-c",
    r#"sleep 0.3; yes "$(printf '\033[2;1Hburst')" | head -c 400000; printf '\033[3;1Hburst done'; sleep 1; printf '\033[4;1Hfinal'; sleep 30"#,
  ]);
  tty.tty = Some((6, 30));
  let proc = host.spawn(tty).unwrap();
  let mut owner = proc.subscribe(true, Subscriber::Owner);
  let mut viewer = proc.subscribe(true, Subscriber::Viewer);
  let mut program = terminal(6, 30);
  tokio::time::timeout(Duration::from_secs(20), render_until(&mut owner.live, &mut program, "burst done")).await.unwrap();

  let mut client = terminal(6, 30);
  tokio::time::timeout(Duration::from_secs(20), render_until(&mut viewer.live, &mut client, "final")).await.unwrap();
  tokio::time::timeout(Duration::from_secs(20), render_until(&mut owner.live, &mut program, "final")).await.unwrap();

  assert!(!viewer.fell_behind());
  assert_eq!(client.screen().contents(), program.screen().contents());
  proc.send(Input::Signal("SIGKILL".into()));
}
