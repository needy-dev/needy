use super::*;

const BOB: Account<'static> = Account {
  name: "bob",
  uid: 1001,
  gid: 1001,
};

// An Ubuntu image's files after `useradd agent` (uid 1000), as needy's actor
// image has them, shortened.
const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\nagent:x:1000:1000::/home/agent:/bin/bash\n";
const GROUP: &str = "root:x:0:\nagent:x:1000:\n";

#[test]
fn a_new_user_is_appended() {
  assert_eq!(passwd(PASSWD, &BOB), format!("{PASSWD}bob:x:1001:1001::/home/bob:/bin/bash\n"));
  assert_eq!(group(GROUP, &BOB), format!("{GROUP}bob:x:1001:\n"));
}

#[test]
fn an_entry_with_the_same_uid_is_replaced() {
  // Ubuntu's own images have `ubuntu` at uid 1000.
  let ubuntu = "root:x:0:0:root:/root:/bin/bash\nubuntu:x:1000:1000:Ubuntu:/home/ubuntu:/bin/bash\n";
  let agent = Account {
    name: "agent",
    uid: 1000,
    gid: 1000,
  };
  assert_eq!(passwd(ubuntu, &agent), "root:x:0:0:root:/root:/bin/bash\nagent:x:1000:1000::/home/agent:/bin/bash\n");
  assert_eq!(group("root:x:0:\nubuntu:x:1000:\n", &agent), "root:x:0:\nagent:x:1000:\n");
}

#[test]
fn an_entry_with_the_same_name_and_another_id_is_replaced() {
  let renumbered = Account {
    name: "agent",
    uid: 1001,
    gid: 1001,
  };
  assert_eq!(passwd(PASSWD, &renumbered), "root:x:0:0:root:/root:/bin/bash\nagent:x:1001:1001::/home/agent:/bin/bash\n");
}

#[test]
fn a_file_with_the_entry_is_unchanged() {
  let once = passwd(PASSWD, &BOB);
  assert_eq!(passwd(&once, &BOB), once);
  let once = group(GROUP, &BOB);
  assert_eq!(group(&once, &BOB), once);
}

#[test]
fn a_file_without_a_final_newline_keeps_its_lines() {
  assert_eq!(group("root:x:0:", &BOB), "root:x:0:\nbob:x:1001:\n");
}

#[test]
fn shadow_gets_a_locked_entry_unless_the_name_has_one() {
  let shadow_text = "root:*:19000:0:99999:7:::\n";
  assert_eq!(shadow(shadow_text, &BOB), format!("{shadow_text}bob:!:::::::\n"));
  let with_bob = "root:*:19000:0:99999:7:::\nbob:$6$x:19000:0:99999:7:::\n";
  assert_eq!(shadow(with_bob, &BOB), with_bob);
}
