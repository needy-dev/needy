use std::path::PathBuf;

use super::*;

fn proxy(url: &str) -> Result<Upstream, String> {
  Upstream::socks5(url, None).map_err(|e| e.to_string())
}

#[test]
fn a_socks_url_names_who_resolves_and_the_proxy_with_port_1080_by_default() {
  assert_eq!(proxy("socks5h://proxy.example.com:1081").unwrap().via().as_deref(), Some("socks5h://proxy.example.com:1081"));
  assert_eq!(proxy("socks5://proxy.example.com").unwrap().via().as_deref(), Some("socks5://proxy.example.com:1080"));
  assert_eq!(proxy("socks5h://[2001:db8::1]:1080/").unwrap().via().as_deref(), Some("socks5h://[2001:db8::1]:1080"));
  assert_eq!(Upstream::Direct.via(), None);
}

#[test]
fn a_socks_url_with_credentials_another_scheme_or_a_path_is_refused_without_echoing_it() {
  let error = proxy("socks5h://user:hunter2@proxy.example.com:1080").unwrap_err();
  assert!(error.contains("credentials in the URL are refused"), "{error}");
  assert!(!error.contains("hunter2"), "{error}");
  assert!(proxy("http://proxy.example.com:3128").unwrap_err().contains("socks5h:// or socks5://"));
  assert!(proxy("socks5h://proxy.example.com:1080/path").unwrap_err().contains("no path"));
  assert!(proxy("socks5h:///").is_err());
}

#[test]
fn a_credentials_file_holds_one_username_and_password() {
  let parsed = parse_credentials("agent-7:p@ss:word\n").unwrap();
  // The username ends at the first colon; the password may hold more.
  assert_eq!((parsed.username.as_str(), parsed.password.as_str()), ("agent-7", "p@ss:word"));
  assert!(parse_credentials("agent-7:pw\r\n").is_ok());
  for bad in [
    "",
    "agent-7",
    ":pw",
    "agent-7:",
    "a:b\nc:d",
    &format!("{}:pw", "u".repeat(256)),
  ] {
    let error = parse_credentials(bad).err().unwrap_or_else(|| panic!("{bad:?} was accepted"));
    assert!(!error.contains("pw"), "the reason repeats the file's text: {error}");
  }
}

fn write(dir: &std::path::Path, text: &str, mode: u32) -> PathBuf {
  use std::os::unix::fs::PermissionsExt as _;
  let path = dir.join("credentials");
  // A read-only file from an earlier call cannot be written over; on the
  // first call there is none, which needs no removal.
  let _ = std::fs::remove_file(&path);
  std::fs::write(&path, text).unwrap();
  std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
  path
}

#[tokio::test]
async fn a_credentials_file_others_can_read_is_refused_and_group_read_is_allowed() {
  let dir = tempfile::tempdir().unwrap();
  let check = |mode| {
    let path = write(dir.path(), "agent-7:s3cret-value\n", mode);
    async move { Upstream::socks5("socks5h://proxy.example.com", Some(path)).unwrap().check().await.map_err(|e| e.to_string()) }
  };

  assert!(check(0o600).await.is_ok());
  // A Kubernetes Secret with `defaultMode: 0440` read through `fsGroup`.
  assert!(check(0o440).await.is_ok());
  let error = check(0o644).await.unwrap_err();
  assert!(error.contains("other users can access it (mode 644)"), "{error}");
  assert!(!error.contains("s3cret-value"), "{error}");
}

// A Kubernetes Secret's file is a symlink into `..data/`, which the kubelet
// swaps on rotation: the path is followed, and read again each time.
#[tokio::test]
async fn a_credentials_file_behind_a_swapped_symlink_is_read_again() {
  let dir = tempfile::tempdir().unwrap();
  let first = dir.path().join("first");
  let second = dir.path().join("second");
  for (path, text) in [(&first, "alice:one\n"), (&second, "bob:two\n")] {
    std::fs::create_dir(path).unwrap();
    write(path, text, 0o440);
  }
  let link = dir.path().join("..data");
  std::os::unix::fs::symlink(&first, &link).unwrap();
  let path = dir.path().join("credentials");
  std::os::unix::fs::symlink(link.join("credentials"), &path).unwrap();
  let Upstream::Socks5(proxy) = Upstream::socks5("socks5h://proxy.example.com", Some(path)).unwrap() else {
    unreachable!();
  };

  assert_eq!(proxy.credentials().await.unwrap().unwrap().username, "alice");
  let swapped = dir.path().join("..data_tmp");
  std::os::unix::fs::symlink(&second, &swapped).unwrap();
  std::fs::rename(&swapped, &link).unwrap();
  assert_eq!(proxy.credentials().await.unwrap().unwrap().username, "bob");
}
