use super::*;

// The messages below are the kernel's, as `log stream --style ndjson`
// printed them on macOS 27.2 for a profile tagged `needy-test1` (process IDs
// and paths as captured; nothing private in them).

#[test]
fn an_exec_denial_names_the_process_and_the_program() {
  let deny = parse("Sandbox: bash(72949) deny(1) process-exec* /bin/ls\nneedy-test1").unwrap();
  assert_eq!(
    deny,
    ActorDeny {
      process: "bash".into(),
      pid: 72949,
      operation: "process-exec*".into(),
      target: "/bin/ls".into(),
    }
  );
}

#[test]
fn a_network_denial_names_the_address() {
  let deny = parse("Sandbox: python3.9(5) deny(1) network-outbound 1.1.1.1:443\nneedy-r1").unwrap();
  assert_eq!((deny.operation.as_str(), deny.target.as_str()), ("network-outbound", "1.1.1.1:443"));
}

#[test]
fn a_process_name_with_spaces_and_parentheses_is_kept_whole() {
  let deny = parse("Sandbox: Code Helper (Plugin)(77) deny(1) file-write-create /private/tmp/x\nneedy-r1").unwrap();
  assert_eq!((deny.process.as_str(), deny.pid), ("Code Helper (Plugin)", 77));
}

// sandboxd's report for a program that is not Apple's: the denial, the tag,
// then the report (shortened here; captured on macOS 27.2 for opencode).
#[test]
fn a_violation_report_is_read_from_its_first_line() {
  let message =
    "Sandbox: opencode(38139) deny(1) process-exec* /bin/sh\nneedy-r1\nProcess:         opencode [38139]\nMetaData: {\"errno\":1}";
  let deny = parse(message).unwrap();
  assert_eq!((deny.process.as_str(), deny.operation.as_str(), deny.target.as_str()), ("opencode", "process-exec*", "/bin/sh"));
}

#[test]
fn other_messages_are_not_denials() {
  assert_eq!(
    parse("log run noninteractively, parent: 1 (needy), args: '/usr/bin/log' 'stream' '--predicate' 'eventMessage CONTAINS \"needy-r1\"'"),
    None
  );
  assert_eq!(parse("1 duplicate report for Sandbox: python3(72951) deny(1) file-write-data /dev/dtracehelper\nneedy-test1"), None);
  assert_eq!(parse("needy-test1"), None);
}
