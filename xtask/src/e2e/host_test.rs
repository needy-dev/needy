use super::*;

// ROOT CAUSE:
//
// If two checkouts on one host ran the harness (a worktree beside the main
// checkout), each one's cells booted the images the other had built last,
// because every checkout tagged its images `latest` while each kept its
// freshness stamps in its own work directory, so a stamp said fresh for an
// image the other checkout had replaced.
//
// Before the fix, every checkout used `needy-e2e-actor:latest`.
// The fix tags the images with the checkout's directory name.
#[test]
fn two_checkouts_build_images_of_their_own() {
  let main = Images::of_checkout(Path::new("/Users/user/src/needy"));
  let worktree = Images::of_checkout(Path::new("/tmp/needy-main-wt"));

  assert_eq!(main.actor, "needy-e2e-actor:needy");
  assert_eq!(main.executor, "needy-e2e-executor:needy");
  assert_eq!(main.agents, "needy-e2e-agents:needy");
  assert_eq!(worktree.actor, "needy-e2e-actor:needy-main-wt");
}

// A run host runs the harness its build host compiled in `DIR/src`, so both
// name the images the run host loads alike.
#[test]
fn a_shipped_tree_names_its_images_after_src() {
  assert_eq!(Images::shipped(), Images::of_checkout(Path::new("/home/user/needy-e2e/src")));
}

#[test]
fn a_checkout_name_becomes_a_valid_tag() {
  assert_eq!(tag("needy"), "needy");
  assert_eq!(tag("my repo@2"), "my-repo-2");
  assert_eq!(tag(".hidden"), "_.hidden");
  assert_eq!(tag(""), "_");
  assert_eq!(tag(&"x".repeat(200)).len(), 128);
}
