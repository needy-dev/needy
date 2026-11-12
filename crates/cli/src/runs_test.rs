use needy_api_client::proto::v1;

use super::*;

fn run(id: &str, name: &str, state: v1::RunState, created_at_ms: i64) -> Run {
  Run {
    run_id: id.into(),
    name: name.into(),
    state: state as i32,
    created_at_ms,
    profile: "opencode".into(),
    ..Default::default()
  }
}

fn runs() -> Vec<Run> {
  vec![
    run("aaa111", "aaa111", v1::RunState::Stopped, 1),
    run("abc222", "review", v1::RunState::Running, 3),
    run("abd333", "abd333", v1::RunState::Finished, 2),
  ]
}

#[test]
fn latest_is_the_run_created_last_whatever_its_state() {
  assert_eq!(find(&runs(), "latest").unwrap(), "abc222");
}

#[test]
fn a_prefix_or_a_name_names_one_run() {
  assert_eq!(find(&runs(), "aa").unwrap(), "aaa111");
  assert_eq!(find(&runs(), "abd").unwrap(), "abd333");
  assert_eq!(find(&runs(), "review").unwrap(), "abc222");
}

#[test]
fn a_prefix_of_several_runs_lists_them() {
  let error = find(&runs(), "ab").unwrap_err().to_string();

  assert!(error.starts_with("run ab is ambiguous: 2 runs match"), "{error}");
  assert!(error.contains("abc222") && error.contains("abd333") && !error.contains("aaa111"), "{error}");
}

#[test]
fn an_unknown_run_is_an_error() {
  assert_eq!(find(&runs(), "zz").unwrap_err().to_string(), "no run zz");
  assert!(find(&[], "latest").unwrap_err().to_string().starts_with("there is no run yet"));
}

#[test]
fn the_only_live_run_is_taken_and_ended_runs_do_not_count() {
  assert_eq!(only_live(&runs()).unwrap(), "abc222");
}

#[test]
fn no_live_run_is_an_error() {
  let ended = vec![run("aaa111", "aaa111", v1::RunState::Failed, 1)];

  assert!(only_live(&ended).unwrap_err().to_string().starts_with("no run is live"));
}

#[test]
fn several_live_runs_are_listed_with_their_id_profile_and_age() {
  let mut live = runs();
  live.push(run("fff444", "fff444", v1::RunState::Creating, 4));

  let error = only_live(&live).unwrap_err().to_string();

  let lines: Vec<&str> = error.lines().collect();
  assert!(lines[0].starts_with("2 runs are live"), "{error}");
  assert!(lines[1].split_whitespace().eq(["ID", "PROFILE", "AGE"]), "{error}");
  assert!(lines[2].trim_start().starts_with("abc222  opencode  ") && lines[2].ends_with(" ago"), "{error}");
  assert!(lines[3].trim_start().starts_with("fff444  opencode  "), "{error}");
}
