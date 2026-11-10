use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

/// A tar with one file `name` holding `content`, with mode `mode`.
fn archive_with(dir: &Path, name: &str, content: &[u8], mode: u32) -> PathBuf {
  let path = dir.join("image.tar");
  let mut tar = tar::Builder::new(std::fs::File::create(&path).unwrap());
  let mut header = tar::Header::new_gnu();
  header.set_size(content.len() as u64);
  header.set_mode(mode);
  header.set_entry_type(tar::EntryType::Regular);
  header.set_cksum();
  tar.append_data(&mut header, name, content).unwrap();
  tar.finish().unwrap();
  path
}

// ROOT CAUSE:
//
// If the image's tar held a file with long runs of zeros (the actor's
// `rootfs.ext4`, mostly free space), the unpacked file took its full length
// on disk because the tar was unpacked by writing every byte.
//
// Before the fix, a 3 GiB rootfs with about 560 MB in use took 3.1 GB on disk.
// The fix keeps zero blocks as holes, so the file takes the space of its data.
#[test]
fn unpack_leaves_runs_of_zeros_as_holes() {
  let dir = tempfile::tempdir().unwrap();
  // NOTICE: APFS stores a skipped range of up to about 16 MiB as zeros (a
  // 64 MiB one became a hole when probed on an APFS volume); Linux file systems
  // leave any skipped block unallocated.
  let zeros = 64 << 20;
  let mut content = vec![0u8; zeros + 2 * 4096];
  content[..4096].fill(b'a');
  content[zeros + 4096..].fill(b'b');
  let archive = archive_with(dir.path(), "rootfs.ext4", &content, 0o644);
  let out = dir.path().join("out");
  std::fs::create_dir(&out).unwrap();

  unpack(&archive, &out).unwrap();

  let unpacked = out.join("rootfs.ext4");
  assert_eq!(std::fs::read(&unpacked).unwrap(), content);
  let allocated = std::fs::metadata(&unpacked).unwrap().blocks() * 512;
  assert!(allocated < 4 << 20, "{allocated} bytes allocated for {} bytes", content.len());
}

#[test]
fn unpack_ends_a_file_with_a_hole_at_its_full_length() {
  let dir = tempfile::tempdir().unwrap();
  let mut content = vec![0u8; 1 << 20];
  content[..10].fill(b'x');
  let archive = archive_with(dir.path(), "tail", &content, 0o644);
  let out = dir.path().join("out");
  std::fs::create_dir(&out).unwrap();

  unpack(&archive, &out).unwrap();

  assert_eq!(std::fs::read(out.join("tail")).unwrap(), content);
}

#[test]
fn unpack_keeps_the_file_mode() {
  let dir = tempfile::tempdir().unwrap();
  let archive = archive_with(dir.path(), "firecracker", b"#!/bin/sh\n", 0o755);
  let out = dir.path().join("out");
  std::fs::create_dir(&out).unwrap();

  unpack(&archive, &out).unwrap();

  let mode = std::fs::metadata(out.join("firecracker")).unwrap().permissions().mode();
  assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn unpack_overwrites_an_earlier_build() {
  let dir = tempfile::tempdir().unwrap();
  let out = dir.path().join("out");
  std::fs::create_dir(&out).unwrap();
  std::fs::write(out.join("vmlinux"), vec![b'o'; 8192]).unwrap();
  let archive = archive_with(dir.path(), "vmlinux", b"new", 0o644);

  unpack(&archive, &out).unwrap();

  assert_eq!(std::fs::read(out.join("vmlinux")).unwrap(), b"new");
}

/// The `ARG`s the `kernel` stage of `images/Dockerfile` declares.
fn kernel_stage_args() -> Vec<String> {
  let dockerfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/Dockerfile");
  let text = std::fs::read_to_string(dockerfile).unwrap();
  text
    .lines()
    .skip_while(|line| !line.starts_with("FROM ") || !line.ends_with(" AS kernel"))
    .skip(1)
    .take_while(|line| !line.starts_with("FROM "))
    .filter_map(|line| line.strip_prefix("ARG "))
    .map(|arg| arg.split('=').next().unwrap().to_string())
    .collect()
}

// ROOT CAUSE:
//
// If a recipe set `os.kernel.release` or `version`, the actor's `uname` did
// not show it, because `needy build` passed them as build arguments the
// Dockerfile's kernel stage never declared, and BuildKit drops undeclared
// build arguments without an error.
//
// Before the fix, `NEEDY_KERNEL_RELEASE` and `NEEDY_KERNEL_VERSION` were
// passed and ignored.
// The fix keeps every kernel build variable a recipe yields declared in the
// kernel stage.
#[test]
fn every_kernel_build_variable_of_a_recipe_is_an_arg_of_the_kernel_stage() {
  let args = kernel_stage_args();
  for name in needy_config::builtin::recipes() {
    let recipe = needy_config::recipe::load(&Source::builtin(name)).unwrap();
    let compiled = needy_core::recipe::compile(&recipe, &needy_core::recipe::HostInfo::default(), 0);
    assert!(compiled.kernel_build_env.contains_key("KERNELRELEASE"), "{name}");
    for key in compiled.kernel_build_env.keys() {
      assert!(args.contains(key), "{name}: {key} is not an ARG of the kernel stage: {args:?}");
    }
  }
}
