use super::*;

/// Synthetic registration for native command output; no service is installed.
fn registration() -> Registration {
  Registration {
    version: 1,
    executable: PathBuf::from("bin").join("needy"),
    snapshot: PathBuf::from("config").join("control.yaml"),
    definition: PathBuf::from("service"),
    runtime: PathBuf::from("runtime"),
    state: PathBuf::from("state"),
    data: PathBuf::from("data"),
    configuration: PathBuf::from("config"),
    account: "user".into(),
    uid: 1000,
    scope: Scope::User,
    path: String::new(),
    engine_host: None,
    log: PathBuf::from("state with spaces").join("service.log"),
  }
}

#[test]
fn systemd_status_hints_target_the_installed_scope_and_unit() {
  let registration = registration();
  let user = systemd::Manager::new(Scope::User);
  let system = systemd::Manager::new(Scope::System);

  assert_eq!(command_line(&user.status_command(&registration)), "systemctl --user status needy.service");
  assert_eq!(command_line(&system.status_command(&registration)), "systemctl status needy.service");
  assert_eq!(system.definition().unwrap(), PathBuf::from("/etc/systemd/system/needy.service"));
  assert_eq!(command_line(&system.logs(&registration, true, 100)), "journalctl '--unit=needy.service' --no-pager --lines 100 --follow");
}

#[test]
fn launchd_status_hints_name_the_domain_and_keep_log_paths_copyable() {
  let registration = registration();
  let user = launchd::Manager::new(Scope::User);
  let system = launchd::Manager::new(Scope::System);

  assert_eq!(command_line(&user.status_command(&registration)), "launchctl print gui/1000/dev.needy.control");
  assert_eq!(command_line(&system.status_command(&registration)), "launchctl print system/dev.needy.control");
  assert_eq!(command_line(&user.logs(&registration, true, 100)), "tail -n 100 -f 'state with spaces/service.log'");
}
