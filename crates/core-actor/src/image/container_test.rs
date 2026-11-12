use super::*;

#[test]
fn an_image_of_the_guests_platform_is_accepted() {
  check_platform("needy-actor:latest", "linux", "amd64", "x86_64").unwrap();
  check_platform("needy-actor:latest", "linux", "arm64", "aarch64").unwrap();
}

#[test]
fn an_image_of_another_architecture_is_refused_with_the_platform_to_build() {
  let error = check_platform("needy-custom-jq", "linux", "amd64", "aarch64").unwrap_err();

  assert_eq!(
    error,
    "the actor image needy-custom-jq is linux/amd64, and actors here run linux/arm64, the guest needy's platform; build it for \
     linux/arm64 (`docker build --platform linux/arm64`)"
  );
}

#[test]
fn the_base_image_is_rebuilt_with_needy_and_a_custom_one_from_a_base() {
  assert_eq!(advice("needy-actor:latest", "needy build --linux-container"), base::rebuild_with("needy build --linux-container"));
  assert!(advice("example.com/agent:1", "needy build --linux-container").starts_with("rebuild it FROM a base image of base ABI 1"));
}
