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
  // The executor resolves the program on its own path.
  assert_eq!(route.argv, ["python3", "/srv/ws/a.py"]);
  assert_eq!(route.cwd, "/srv/ws/sub");
  assert_eq!(route.env.get("ROUTED").map(String::as_str), Some("1"));
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
  assert_eq!(route.cwd, "/home/agent/workspace");
}

#[tokio::test]
async fn a_denied_command_is_refused_with_the_rules_message() {
  let route = route(&["rm", "-rf", "/"], "/").await;
  assert_eq!(route.target, Target::Refused("denied: no deleting here".into()));
}
