//! The kernel capture of a VM actor: `binfmt_needy`
//! (`images/kernel/binfmt_needy.c`) runs ahead of `binfmt_elf` and
//! `binfmt_script` and hands a routed exec to the proxy, by any path. Init
//! turns it on by setting the module's parameters, and sets `route` again
//! for each route list the daemon sends; until then `route` is empty and
//! nothing is routed.

use super::Route;

/// The module's parameters, as `module_param` exposes them.
// Source: images/kernel/binfmt_needy.c (`module_param_string(route, ...)`,
// `module_param(route_uid, int, 0600)`): `route` is the names
// comma-separated, or `*`; `route_uid` the user whose execs are routed.
// Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/ABI/stable/sysfs-module
// (a parameter is `/sys/module/<module>/parameters/<name>`).
const ROUTE_UID: &str = "/sys/module/binfmt_needy/parameters/route_uid";
const ROUTE: &str = "/sys/module/binfmt_needy/parameters/route";

/// Turns routing on for `route`. A parameter the kernel lacks is logged and
/// skipped; the actor still runs, without routing.
pub fn enable(route: &Route) {
  write(ROUTE_UID, &route.uid.to_string());
  write(ROUTE, &route.names.join(","));
}

fn write(path: &str, value: &str) {
  if let Err(e) = std::fs::write(path, value) {
    tracing::warn!("cannot write {path}: {e}");
  }
}
