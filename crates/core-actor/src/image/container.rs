//! The container actor image (`needy build --linux-container` or
//! `--macos-apple-container`, `needy_config::profile::CONTAINER_IMAGE` by
//! default) as the container drivers run it: one container per run, named
//! after the run.
// NOTICE: the image's shim layout (`/.needy/proxy`, `/.needy/real/`) is the
// guest's (`guest/shim.rs`); it moves with the captures in slice 5 of the
// drivers design.

/// The container of run `run`'s actor.
pub fn name(run: &str) -> String {
  format!("needy-actor-{run}")
}
