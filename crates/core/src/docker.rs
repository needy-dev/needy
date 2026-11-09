//! Docker run options a profile may pass to an executor container.
//!
//! TODO(core-executor): this policy moves to `core-executor::provider::docker`
//! with the bollard provider in slice 5 of the module layout redesign.

const DAEMON_OWNED: &[&str] = &["--network", "--net", "--name", "--label", "-l"];
const VALUE_FLAGS: &[&str] = &[
  "--network",
  "--net",
  "--name",
  "--label",
  "-l",
  "--cap-add",
  "--device",
  "--pid",
  "--ipc",
  "--userns",
  "--uts",
  "--security-opt",
  "-v",
  "--volume",
  "--mount",
];

/// Checks docker run options from a profile. Fields the daemon owns are always
/// refused; privileged options pass only when `operator_allow` names them
/// (`--privileged`, `--cap-add`, `--device`, `--pid`, `--ipc`, `--userns`,
/// `--uts`, `--security-opt`, `host-root`).
pub fn check_docker_args(args: &[String], operator_allow: &[String]) -> Result<(), String> {
  let allowed = |name: &str| operator_allow.iter().any(|a| a == name);
  let mut i = 0;
  while i < args.len() {
    let arg = &args[i];
    let (flag, inline) = match arg.split_once('=') {
      Some((f, v)) if f.starts_with('-') => (f.to_string(), Some(v.to_string())),
      _ => (arg.clone(), None),
    };
    let value = if inline.is_some() {
      inline
    } else if VALUE_FLAGS.contains(&flag.as_str()) {
      i += 1;
      args.get(i).cloned()
    } else {
      None
    };
    i += 1;
    if DAEMON_OWNED.contains(&flag.as_str()) {
      return Err(format!("{flag} is set by the daemon"));
    }
    let value = value.unwrap_or_default();
    match flag.as_str() {
      "--privileged" | "--cap-add" | "--device" if !allowed(&flag) => {
        return Err(format!("{flag} is not allowed by the operator"));
      }
      "--pid" | "--ipc" | "--userns" | "--uts" if value == "host" && !allowed(&flag) => {
        return Err(format!("{flag} host is not allowed by the operator"));
      }
      "--security-opt" if value.contains("unconfined") && !allowed(&flag) => {
        return Err(format!("{flag} {value} is not allowed by the operator"));
      }
      "-v" | "--volume" => {
        let mut parts = value.split(':');
        let source = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        check_mount(source, target, &allowed)?;
      }
      "--mount" => {
        let mut source = "";
        let mut target = "";
        for pair in value.split(',') {
          match pair.split_once('=') {
            Some(("source" | "src", v)) => source = v,
            Some(("target" | "dst" | "destination", v)) => target = v,
            _ => {}
          }
        }
        check_mount(source, target, &allowed)?;
      }
      _ => {}
    }
  }
  Ok(())
}

fn check_mount(source: &str, target: &str, allowed: &dyn Fn(&str) -> bool) -> Result<(), String> {
  if target.split('/').any(|part| part == "workspace") {
    return Err(format!("the workspace mount at {target} is set by the daemon"));
  }
  if source == "/" && !allowed("host-root") {
    return Err("mounting the host root is not allowed by the operator".into());
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn docker_args_are_checked() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(check_docker_args(&a(&["--cpus=4", "--memory", "8g"]), &[]).is_ok());
    assert!(check_docker_args(&a(&["--network", "host"]), &[]).is_err());
    assert!(check_docker_args(&a(&["--name=x"]), &[]).is_err());
    assert!(check_docker_args(&a(&["--privileged"]), &[]).is_err());
    assert!(check_docker_args(&a(&["--privileged"]), &a(&["--privileged"])).is_ok());
    assert!(check_docker_args(&a(&["--pid", "host"]), &[]).is_err());
    assert!(check_docker_args(&a(&["--pid", "container:x"]), &[]).is_ok());
    assert!(check_docker_args(&a(&["-v", "/:/host"]), &[]).is_err());
    assert!(check_docker_args(&a(&["-v", "/:/host"]), &a(&["host-root"])).is_ok());
    assert!(check_docker_args(&a(&["-v", "/data:/home/agent/workspace"]), &[]).is_err());
    assert!(check_docker_args(&a(&["--mount", "type=bind,source=/,target=/h"]), &[]).is_err());
    assert!(check_docker_args(&a(&["-v", "/cache:/cache:ro"]), &[]).is_ok());
    assert!(check_docker_args(&a(&["--security-opt", "seccomp=unconfined"]), &[]).is_err());
  }
}
