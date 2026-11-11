use needy_common::process::Exit;

use super::*;

fn failed(stderr: &str) -> needy_api_client::Error {
  needy_api_client::Error::Failed {
    exit: Exit {
      code: Some(1),
      signal: String::new(),
    },
    stderr: stderr.into(),
  }
}

// ROOT CAUSE:
//
// If `needy fs cp` copied a missing source from a Windows executor, its
// bsdtar said `tar: : Couldn't visit directory`, naming no path, and the
// client passed that on with only the executor's name.
//
// Before the fix, the message was `tar on windows failed: tar: : Couldn't
// visit directory`.
// The fix names the path the copy asked for, whatever tar says.
#[test]
fn a_failed_copy_names_the_executor_and_the_path() {
  let error = tar_failed("windows", "C:/Users/user/missing", failed("tar: : Couldn't visit directory\r\n"));

  assert_eq!(error.to_string(), "cannot copy windows:C:/Users/user/missing: tar: : Couldn't visit directory");
}
