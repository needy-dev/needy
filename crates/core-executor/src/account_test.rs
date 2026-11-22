use crate::engine::testing::Fake;

use super::*;

// ROOT CAUSE:
//
// If a profile renamed its user (`bob`), the container's `/etc/passwd`
// still had only the image's user at that uid, so `HOME` was `/home/bob`
// while `whoami` said the image's user (or failed for an unknown uid).
//
// Before the fix, the image's account files were used as they were.
// The fix binds account files rewritten for the profile's user.
#[tokio::test]
async fn the_image_files_are_rewritten_for_the_user_and_bound_read_only() {
  let fake = Fake::new()
    .require_image_on_create()
    .with_file("/etc/passwd", "root:x:0:0:root:/root:/bin/bash\nubuntu:x:1000:1000::/home/ubuntu:/bin/bash\n")
    .with_file("/etc/group", "root:x:0:\nubuntu:x:1000:\n");
  let dir = tempfile::tempdir().unwrap();
  let user = Account {
    name: "bob",
    uid: 1000,
    gid: 1000,
  };

  let binds = binds(&fake, "needy-executor:latest", "needy-x-account", dir.path(), &user).await.unwrap();

  let passwd = dir.path().join("passwd");
  let group = dir.path().join("group");
  assert_eq!(
    binds,
    [
      format!("{}:/etc/passwd:ro", passwd.display()),
      format!("{}:/etc/group:ro", group.display())
    ]
  );
  assert_eq!(std::fs::read_to_string(passwd).unwrap(), "root:x:0:0:root:/root:/bin/bash\nbob:x:1000:1000::/home/bob:/bin/bash\n");
  assert_eq!(std::fs::read_to_string(group).unwrap(), "root:x:0:\nbob:x:1000:\n");
  assert_eq!(
    fake.calls(),
    [
      "remove needy-x-account",
      "create needy-x-account",
      "pull needy-executor:latest",
      "create needy-x-account",
      "download needy-x-account /etc/passwd",
      "download needy-x-account /etc/group",
      "download needy-x-account /etc/shadow",
      "remove needy-x-account",
    ]
  );
}
