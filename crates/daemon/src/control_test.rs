use super::*;

#[test]
fn a_profile_is_named_after_its_directory() {
  assert_eq!(profile_name(Path::new("/home/user/needy/examples/smoke")), "smoke");
}
