use super::*;

#[test]
fn a_driver_is_named_as_a_profile_names_it() {
  assert_eq!(driver("macos-vz"), Ok(ActorDriver::MacosVz));
  assert_eq!(
    driver("firecracker"),
    Err("\"firecracker\" is not an actor driver (linux-firecracker, linux-container, macos-vz, macos-apple-container)".into())
  );
}

#[test]
fn the_release_images_are_tagged_with_the_version() {
  assert_eq!(release_image("needy-actor", "0.0.1"), "ghcr.io/needy-dev/needy-actor:v0.0.1");
  assert_eq!(release_image("needy-actor", env!("CARGO_PKG_VERSION")), needy_config::profile::CONTAINER_IMAGE);
  assert_eq!(release_image("needy-executor", env!("CARGO_PKG_VERSION")), needy_core_executor::driver::linux_container::IMAGE);
}
