//! Sidecars: the services needy runs for one run, next to the actor and
//! outside it, that the actor reaches through one of its channels
//! (`crate::driver::Channels`), and that end with the run. Not the VM or
//! container process (its driver owns that) and not init (inside the actor).
//!
//! | Sidecar | Channel | Domain it adapts | Lives in |
//! | --- | --- | --- | --- |
//! | gateway | `gateway` (vsock port or `gateway.sock`) | net (`net.request`, `net.connect`) | [`gateway`], here: mechanism only, it needs no run state beyond its chain |
//! | guest API | `guest` (vsock port or `guest.sock`) | exec (`Runs::decide`, which the actor's proxy calls); configuration, events, exit, files | `needy-daemon`'s `guest` and `run`, which need the run's routing, store and trace |
//!
//! Host-side sshfs mounts of a container actor's workspace are driver work,
//! not a sidecar. See
//! `docs/ai/references/architecture/2026-11-12-drivers-and-extension-points-design.md`.

pub mod gateway;
