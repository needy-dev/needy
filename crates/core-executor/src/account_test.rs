use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use futures_util::stream::BoxStream;

use super::*;

/// An engine whose containers have `files` (path to content), and which
/// has no image until it pulled one.
#[derive(Default)]
struct Fake {
  files: HashMap<&'static str, &'static str>,
  calls: Mutex<Vec<String>>,
}

impl Fake {
  fn record(&self, call: String) {
    self.calls.lock().unwrap().push(call);
  }
}

#[async_trait::async_trait]
impl Engine for Fake {
  async fn runtimes(&self) -> Result<BTreeMap<String, Vec<String>>, Error> {
    Ok(BTreeMap::new())
  }

  async fn image_labels(&self, _image: &str) -> Result<Option<HashMap<String, String>>, Error> {
    Ok(None)
  }

  async fn pull(&self, image: &str) -> Result<(), Error> {
    self.record(format!("pull {image}"));
    Ok(())
  }

  async fn create(&self, name: &str, _body: ContainerCreateBody) -> Result<(), Error> {
    self.record(format!("create {name}"));
    if self.calls.lock().unwrap().iter().any(|c| c.starts_with("pull")) {
      Ok(())
    } else {
      Err(Error::NotFound("No such image".into()))
    }
  }

  async fn start(&self, _name: &str) -> Result<u32, Error> {
    unreachable!("the scratch container never starts")
  }

  fn logs(&self, _name: &str) -> BoxStream<'static, Result<bytes::Bytes, Error>> {
    Box::pin(futures_util::stream::empty())
  }

  async fn wait(&self, _name: &str) -> Result<i64, Error> {
    Ok(0)
  }

  async fn stop(&self, _name: &str, _grace: std::time::Duration) -> Result<(), Error> {
    Ok(())
  }

  async fn remove(&self, name: &str) -> Result<(), Error> {
    self.record(format!("remove {name}"));
    Ok(())
  }

  async fn download(&self, _name: &str, path: &str) -> Result<Option<Vec<u8>>, Error> {
    let Some(text) = self.files.get(path) else {
      return Ok(None);
    };
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path.rsplit('/').next().unwrap(), text.as_bytes()).unwrap();
    Ok(Some(archive.into_inner().unwrap()))
  }
}

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
  let fake = Fake {
    files: HashMap::from([
      ("/etc/passwd", "root:x:0:0:root:/root:/bin/bash\nubuntu:x:1000:1000::/home/ubuntu:/bin/bash\n"),
      ("/etc/group", "root:x:0:\nubuntu:x:1000:\n"),
    ]),
    ..Fake::default()
  };
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
    *fake.calls.lock().unwrap(),
    [
      "remove needy-x-account",
      "create needy-x-account",
      "pull needy-executor:latest",
      "create needy-x-account",
      "remove needy-x-account",
    ]
  );
}
