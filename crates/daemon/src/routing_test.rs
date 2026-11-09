use needy_api_server::control::{Route, Sessions, Target};
use needy_core::intercept::ExecContext;

use crate::testing::{self, new_session};

const PROFILE: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here:
    runtime: native
    path_map: { /home/agent/workspace: /srv/ws }
commands:
  - { match: "rm **", deny: no deleting here }
  - { match: "python3 **", to: here, env: { set: { ROUTED: "1" } } }
  - { match: "tool.sh **", to: here }
"#;

fn ctx(argv: &[&str], cwd: &str) -> ExecContext {
  ExecContext {
    argv: argv.iter().map(|a| a.to_string()).collect(),
    cwd: cwd.into(),
    ..ExecContext::default()
  }
}

async fn route(argv: &[&str], cwd: &str) -> Route {
  let started = testing::daemon().await;
  let created = Sessions::create(&started.daemon, new_session(PROFILE, started.dir.path())).await.unwrap();
  Sessions::decide(&started.daemon, &created.id, ctx(argv, cwd)).await.unwrap()
}

#[tokio::test]
async fn a_routed_command_goes_through_the_executors_path_map() {
  let route = route(&["/usr/bin/python3", "/home/agent/workspace/a.py"], "/home/agent/workspace/sub").await;
  assert_eq!(route.target, Target::Executor("here".into()));
  assert_eq!(route.argv, ["/usr/bin/python3", "/srv/ws/a.py"]);
  assert_eq!(route.cwd, "/srv/ws/sub");
  assert_eq!(route.env.get("ROUTED").map(String::as_str), Some("1"));
}

// ROOT CAUSE:
//
// If a routed command named its program by an absolute path, routing cut it
// to its basename so the executor would find the program on its own PATH.
//
// Before the fix, `/opt/tool/bin/x` could run another `x` from the
// executor's PATH, and a missing program was reported without its path.
// The fix keeps the path and lets the executor search its PATH only when
// the path does not exist there.
#[tokio::test]
async fn a_routed_absolute_program_keeps_its_path_and_may_fall_back_to_the_executors_path() {
  let route = route(&["/usr/bin/python3", "-V"], "/").await;
  assert_eq!(route.target, Target::Executor("here".into()));
  assert_eq!(route.argv, ["/usr/bin/python3", "-V"]);
  assert!(route.search_path_if_missing);
}

#[tokio::test]
async fn a_routed_program_the_path_map_maps_runs_from_the_mapped_path_only() {
  let route = route(&["/home/agent/workspace/tool.sh", "x"], "/").await;
  assert_eq!(route.target, Target::Executor("here".into()));
  assert_eq!(route.argv, ["/srv/ws/tool.sh", "x"]);
  assert!(!route.search_path_if_missing);
}

#[tokio::test]
async fn a_path_map_replaces_whole_path_components_only() {
  let route = route(&["python3", "/home/agent/workspace2/a.py"], "/home/agent/workspace").await;
  assert_eq!(route.argv, ["python3", "/home/agent/workspace2/a.py"]);
  assert_eq!(route.cwd, "/srv/ws");
}

#[tokio::test]
async fn a_command_no_rule_routes_stays_on_the_actor_unchanged() {
  let route = route(&["/bin/ls", "/home/agent/workspace"], "/home/agent/workspace").await;
  assert_eq!(route.target, Target::Actor);
  assert_eq!(route.argv, ["/bin/ls", "/home/agent/workspace"]);
  assert!(!route.search_path_if_missing);
  assert_eq!(route.cwd, "/home/agent/workspace");
}

#[tokio::test]
async fn a_denied_command_is_refused_with_the_rules_message() {
  let route = route(&["rm", "-rf", "/"], "/").await;
  assert_eq!(route.target, Target::Refused("denied: no deleting here".into()));
}

// ROOT CAUSE:
//
// If the daemon built the rules' context without the ancestry `Decide`
// carries, a `when.under` rule could never match a command the actor asked
// about, because the rules saw no parent processes.
//
// Before slice 7a, the daemon never filled `ancestors`.
// This keeps the ancestry from `Decide` reaching the rules through routing.
#[tokio::test]
async fn an_under_rule_matches_the_ancestry_decide_carries() {
  const UNDER: &str = r#"
user: { name: agent, uid: 1000, gid: 1000 }
executors:
  here: { runtime: native }
commands:
  - { match: "python3 **", to: here }
  - { match: "python3 **", when: { under: "bash **" }, deny: under bash }
"#;
  let started = testing::daemon().await;
  let created = Sessions::create(&started.daemon, new_session(UNDER, started.dir.path())).await.unwrap();
  let mut asked = ctx(&["python3", "-c", "1"], "/");
  asked.ancestors = vec![vec!["sh".into(), "-c".into(), "python3 -c 1".into()]];
  let routed = Sessions::decide(&started.daemon, &created.id, asked.clone()).await.unwrap();
  assert_eq!(routed.target, Target::Executor("here".into()));

  asked.ancestors.insert(0, vec!["bash".into(), "-c".into(), "python3 -c 1".into()]);
  let refused = Sessions::decide(&started.daemon, &created.id, asked).await.unwrap();
  assert_eq!(refused.target, Target::Refused("denied: under bash".into()));
}
