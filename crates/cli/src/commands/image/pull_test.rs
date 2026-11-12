use std::io::Write as _;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// A gzip-compressed tar of `files`, as the release workflow packs a VM
/// image.
fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
  let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
  for (name, content) in files {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, name, *content).unwrap();
  }
  tar.into_inner().unwrap().finish().unwrap()
}

/// The manifest the release workflow writes into an archive.
fn packed_manifest(driver: &str, arch: &str, base_abi: u32) -> Vec<u8> {
  format!(r#"{{"version":"0.0.9","driver":"{driver}","arch":"{arch}","base_abi":{base_abi}}}"#).into_bytes()
}

/// A release's `linux-firecracker` archive of base ABI `base_abi`.
fn firecracker_archive(base_abi: u32) -> Vec<u8> {
  let manifest = packed_manifest("linux-firecracker", "x86_64", base_abi);
  archive(&[
    ("vmlinux", b"kernel"),
    ("rootfs.ext4", b"rootfs"),
    ("firecracker", b"vmm"),
    ("manifest.json", &manifest),
  ])
}

fn sha256(bytes: &[u8]) -> String {
  hex(&Sha256::digest(bytes))
}

/// Serves `files` by name over HTTP on loopback while the test runs, and
/// answers 404 for any other path; returns the base URL.
async fn serve(files: Vec<(String, Vec<u8>)>) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move {
    while let Ok((mut stream, _)) = listener.accept().await {
      let files = files.clone();
      tokio::spawn(async move {
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
          match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
          }
        }
        let request = String::from_utf8_lossy(&request);
        let path = request.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
        let (status, body) = match files.iter().find(|(name, _)| *name == path) {
          Some((_, body)) => ("200 OK", body.clone()),
          None => ("404 Not Found", b"Not Found".to_vec()),
        };
        let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        // A test that stopped reading has its answer already.
        if stream.write_all(head.as_bytes()).await.is_ok() {
          let _ = stream.write_all(&body).await;
        }
      });
    }
  });
  format!("http://{address}")
}

/// A client that never goes through a proxy the environment names: the
/// server is on loopback.
fn client() -> reqwest::Client {
  reqwest::Client::builder().no_proxy().build().unwrap()
}

/// The entries of `dir`, by name, sorted.
fn entries(dir: &Path) -> Vec<String> {
  let mut names: Vec<String> =
    std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
  names.sort();
  names
}

#[tokio::test]
async fn a_release_image_is_verified_and_put_in_its_drivers_base_directory() {
  let images = tempfile::tempdir().unwrap();
  let packed = firecracker_archive(base::ABI);
  let digest = sha256(&packed);
  let name = "needy-actor-linux-firecracker-x86_64.tar.gz";
  let assets = serve(vec![
    (name.into(), packed),
    (format!("{name}.sha256"), format!("{digest}\n").into_bytes()),
  ])
  .await;

  let dir = pull_vm(&client(), images.path(), ActorDriver::LinuxFirecracker, "0.0.9", &assets).await.unwrap();

  assert_eq!(dir, vm::base_dir(images.path(), ActorDriver::LinuxFirecracker));
  assert_eq!(std::fs::read(dir.join("rootfs.ext4")).unwrap(), b"rootfs");
  let manifest = manifest::read(&dir).unwrap().unwrap();
  assert_eq!(
    manifest.source,
    Some(manifest::Source::Release {
      tag: "v0.0.9".into(),
      url: format!("{assets}/{name}"),
    })
  );
  assert_eq!(manifest.sha256, Some(digest));
  assert_eq!(manifest.version, "0.0.9", "the release's version, from its archive");
  assert_eq!(entries(images.path()), ["linux-firecracker"], "no download or staging left");
}

#[tokio::test]
async fn an_archive_that_does_not_match_its_sha256_is_not_installed() {
  let images = tempfile::tempdir().unwrap();
  let name = "needy-actor-linux-firecracker-x86_64.tar.gz";
  let assets = serve(vec![
    (name.into(), firecracker_archive(base::ABI)),
    (format!("{name}.sha256"), format!("{}\n", "0".repeat(64)).into_bytes()),
  ])
  .await;

  let error = pull_vm(&client(), images.path(), ActorDriver::LinuxFirecracker, "0.0.9", &assets).await.unwrap_err().to_string();

  assert!(error.contains(&format!("and its .sha256 says {}; nothing was installed", "0".repeat(64))), "{error}");
  assert_eq!(entries(images.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_version_without_a_release_names_the_build_and_another_version() {
  let images = tempfile::tempdir().unwrap();
  let assets = serve(Vec::new()).await;

  let error = pull_vm(&client(), images.path(), ActorDriver::MacosVz, "0.0.9", &assets).await.unwrap_err().to_string();

  assert_eq!(
    error,
    format!(
      "release v0.0.9 has no needy-actor-macos-vz-aarch64.tar.gz ({assets}/needy-actor-macos-vz-aarch64.tar.gz.sha256: 404 Not \
       Found). A needy without a release (a development build) has no base to pull: build it in a needy checkout with `needy \
       image build`, or pull another release's with `--version V`"
    )
  );
}

#[tokio::test]
async fn a_base_of_another_base_abi_is_refused_and_leaves_nothing() {
  let images = tempfile::tempdir().unwrap();
  let file = images.path().join("needy-actor-linux-firecracker-x86_64.tar.gz");
  std::fs::write(&file, firecracker_archive(base::ABI + 1)).unwrap();

  let error = import(images.path(), None, &file).await.unwrap_err().to_string();

  assert!(error.starts_with(&format!("the linux-firecracker base of needy 0.0.9 has base ABI {}", base::ABI + 1)), "{error}");
  assert_eq!(entries(images.path()), ["needy-actor-linux-firecracker-x86_64.tar.gz"]);
}

#[tokio::test]
async fn an_imported_archive_takes_its_driver_from_its_manifest_and_is_checked_against_its_sha256() {
  let images = tempfile::tempdir().unwrap();
  let packed = archive(&[
    ("Image", b"kernel"),
    ("initrd", b""),
    ("rootfs.ext4", b"rootfs"),
    ("manifest.json", &packed_manifest("macos-vz", "aarch64", base::ABI)),
  ]);
  let file = images.path().join("offline.tar.gz");
  std::fs::write(&file, &packed).unwrap();
  std::fs::write(images.path().join("offline.tar.gz.sha256"), format!("{}  offline.tar.gz\n", sha256(&packed))).unwrap();

  let dir = import(images.path(), None, &file).await.unwrap();

  assert_eq!(dir, vm::base_dir(images.path(), ActorDriver::MacosVz));
  let manifest = manifest::read(&dir).unwrap().unwrap();
  assert_eq!(
    manifest.source,
    Some(manifest::Source::File {
      path: std::fs::canonicalize(&file).unwrap()
    })
  );
  assert_eq!(manifest.sha256, Some(sha256(&packed)));
}

#[tokio::test]
async fn an_imported_archive_whose_sha256_beside_it_differs_is_refused() {
  let images = tempfile::tempdir().unwrap();
  let file = images.path().join("offline.tar.gz");
  std::fs::write(&file, firecracker_archive(base::ABI)).unwrap();
  std::fs::write(images.path().join("offline.tar.gz.sha256"), "0".repeat(64)).unwrap();

  let error = import(images.path(), None, &file).await.unwrap_err().to_string();

  assert!(error.ends_with(&format!("says {}; nothing was installed", "0".repeat(64))), "{error}");
  assert!(!vm::base_dir(images.path(), ActorDriver::LinuxFirecracker).exists());
}

#[tokio::test]
async fn an_archive_of_another_driver_than_the_one_named_is_refused() {
  let images = tempfile::tempdir().unwrap();
  let file = images.path().join("needy-actor-linux-firecracker-x86_64.tar.gz");
  std::fs::write(&file, firecracker_archive(base::ABI)).unwrap();

  let error = import(images.path(), Some(ActorDriver::MacosVz), &file).await.unwrap_err().to_string();

  assert_eq!(error, "the archive holds a linux-firecracker image, not a macos-vz one");
  assert_eq!(entries(images.path()), ["needy-actor-linux-firecracker-x86_64.tar.gz"]);
}

#[tokio::test]
async fn an_archive_without_a_manifest_is_no_vm_image() {
  let images = tempfile::tempdir().unwrap();
  let file = images.path().join("old.tar.gz");
  std::fs::write(
    &file,
    archive(&[
      ("vmlinux", b""),
      ("rootfs.ext4", b""),
      ("firecracker", b""),
      ("base-abi", b"1\n"),
    ]),
  )
  .unwrap();

  let error = import(images.path(), None, &file).await.unwrap_err().to_string();

  assert_eq!(error, format!("{} has no manifest.json: it is not a needy VM image archive", std::fs::canonicalize(&file).unwrap().display()));
}

#[tokio::test]
async fn a_newer_pull_replaces_the_older_image_whole() {
  let images = tempfile::tempdir().unwrap();
  let file = images.path().join("a.tar.gz");
  std::fs::write(&file, firecracker_archive(base::ABI)).unwrap();
  let dir = import(images.path(), None, &file).await.unwrap();
  std::fs::write(dir.join("left-over"), "").unwrap();

  let newer = archive(&[
    ("vmlinux", b"kernel 2"),
    ("rootfs.ext4", b"rootfs 2"),
    ("firecracker", b"vmm"),
    ("manifest.json", &packed_manifest("linux-firecracker", "x86_64", base::ABI)),
  ]);
  let mut file = std::fs::File::create(images.path().join("b.tar.gz")).unwrap();
  file.write_all(&newer).unwrap();
  import(images.path(), None, &images.path().join("b.tar.gz")).await.unwrap();

  assert_eq!(std::fs::read(dir.join("vmlinux")).unwrap(), b"kernel 2");
  assert!(!dir.join("left-over").exists());
  assert_eq!(entries(&images.path().join("linux-firecracker")), ["base"], "the older image is gone");
}
