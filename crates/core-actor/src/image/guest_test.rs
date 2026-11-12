use std::path::Path;

use super::*;

/// An ELF64 little-endian executable with only what `check` reads: the file
/// header and program headers for `machine`, a `PT_INTERP` when
/// `interpreter` is given, a `PT_DYNAMIC` with `needed` `DT_NEEDED` entries.
// Source: the ELF-64 Object File Format, version 1.5 draft 2 (1998),
// sections "ELF Header" (64 bytes) and "Program header table" (56 bytes per
// entry), https://uclibc.org/docs/elf-64-gen.pdf; the constants are
// `object::elf`'s.
fn elf(machine: elf::Machine, interpreter: Option<&str>, needed: usize) -> Vec<u8> {
  let mut segments: Vec<(elf::ProgramType, Vec<u8>)> = vec![(elf::PT_LOAD, Vec::new())];
  if let Some(interpreter) = interpreter {
    segments.push((elf::PT_INTERP, format!("{interpreter}\0").into_bytes()));
  }
  if needed > 0 {
    let mut dynamic = Vec::new();
    for _ in 0..needed {
      dynamic.extend(elf::DT_NEEDED.0.to_le_bytes());
      dynamic.extend(1u64.to_le_bytes());
    }
    dynamic.extend([0u8; 16]);
    segments.push((elf::PT_DYNAMIC, dynamic));
  }
  let phoff = 64u64;
  let mut offset = phoff + 56 * segments.len() as u64;
  let mut out = Vec::new();
  out.extend([0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
  out.extend([0u8; 8]);
  out.extend(elf::ET_EXEC.0.to_le_bytes());
  out.extend(machine.0.to_le_bytes());
  out.extend(1u32.to_le_bytes());
  out.extend(0x40_1000u64.to_le_bytes());
  out.extend(phoff.to_le_bytes());
  out.extend(0u64.to_le_bytes());
  out.extend(0u32.to_le_bytes());
  out.extend(64u16.to_le_bytes());
  out.extend(56u16.to_le_bytes());
  out.extend((segments.len() as u16).to_le_bytes());
  out.extend(64u16.to_le_bytes());
  out.extend([0u8; 4]);
  let mut data: Vec<u8> = Vec::new();
  for (kind, content) in &segments {
    out.extend(kind.0.to_le_bytes());
    out.extend(4u32.to_le_bytes());
    out.extend(offset.to_le_bytes());
    out.extend(0x40_0000u64.to_le_bytes());
    out.extend(0x40_0000u64.to_le_bytes());
    out.extend((content.len() as u64).to_le_bytes());
    out.extend((content.len() as u64).to_le_bytes());
    out.extend(8u64.to_le_bytes());
    offset += content.len() as u64;
    data.extend(content);
  }
  out.extend(data);
  out
}

fn write(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
  let path = dir.join(name);
  std::fs::write(&path, content).unwrap();
  path
}

fn refusal(result: Result<(), Error>) -> String {
  match result {
    Err(Error::Refused { why, .. }) => why,
    other => panic!("not refused: {other:?}"),
  }
}

#[test]
fn a_static_linux_executable_of_the_actors_architecture_is_a_guest() {
  let dir = tempfile::tempdir().unwrap();
  let x86 = write(dir.path(), "x86", &elf(elf::EM_X86_64, None, 0));
  let arm = write(dir.path(), "arm", &elf(elf::EM_AARCH64, None, 0));

  check(&x86, "x86_64").unwrap();
  check(&arm, "aarch64").unwrap();
}

#[test]
fn a_dynamically_linked_needy_is_refused_with_its_interpreter() {
  let dir = tempfile::tempdir().unwrap();
  let path = write(dir.path(), "needy", &elf(elf::EM_X86_64, Some("/lib64/ld-linux-x86-64.so.2"), 0));

  assert_eq!(
    refusal(check(&path, "x86_64")),
    "is dynamically linked (its program interpreter is /lib64/ld-linux-x86-64.so.2); the guest needy must be static, as a musl build is"
  );
}

#[test]
fn a_needy_that_needs_shared_libraries_is_refused() {
  let dir = tempfile::tempdir().unwrap();
  let path = write(dir.path(), "needy", &elf(elf::EM_AARCH64, None, 2));

  assert_eq!(refusal(check(&path, "aarch64")), "needs 2 shared libraries; the guest needy must be static, as a musl build is");
}

#[test]
fn a_needy_for_another_architecture_is_refused() {
  let dir = tempfile::tempdir().unwrap();
  let path = write(dir.path(), "needy", &elf(elf::EM_X86_64, None, 0));

  assert_eq!(refusal(check(&path, "aarch64")), "is built for x86_64, and this machine's actors are aarch64");
}

#[test]
fn a_file_that_is_not_elf_is_refused() {
  // A macOS needy is Mach-O; a script is text.
  let dir = tempfile::tempdir().unwrap();
  let path = write(dir.path(), "needy", b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01 a Mach-O header and more bytes than an ELF header needs ....");

  assert_eq!(refusal(check(&path, "aarch64")), "is not a 64-bit ELF executable; the guest needy is a static Linux build of needy");
}

#[test]
fn a_missing_file_is_refused() {
  let dir = tempfile::tempdir().unwrap();

  assert_eq!(refusal(check(&dir.path().join("needy"), "x86_64")), "does not exist");
}

#[test]
fn a_given_guest_needy_is_used_and_must_be_usable() {
  let dir = tempfile::tempdir().unwrap();
  let exe = write(dir.path(), "needy", b"#!/bin/sh\n");
  write(dir.path(), "needy-linux-x86_64", &elf(elf::EM_X86_64, None, 0));
  let given = write(dir.path(), "given", &elf(elf::EM_X86_64, None, 0));
  let broken = write(dir.path(), "broken", &elf(elf::EM_X86_64, Some("/lib/ld.so"), 0));

  assert_eq!(find(Some(&given), &exe, "x86_64").unwrap(), given);
  // A given one that is refused is an error, not a reason to look elsewhere.
  assert!(matches!(find(Some(&broken), &exe, "x86_64"), Err(Error::Refused { path, .. }) if path == broken));
}

#[test]
fn the_guest_needy_next_to_the_daemon_is_found() {
  let dir = tempfile::tempdir().unwrap();
  let exe = write(dir.path(), "needy", b"#!/bin/sh\n");
  let beside = write(dir.path(), "needy-linux-aarch64", &elf(elf::EM_AARCH64, None, 0));

  assert_eq!(find(None, &exe, "aarch64").unwrap(), beside);
}

#[cfg(target_os = "linux")]
#[test]
fn a_static_daemon_on_linux_is_its_own_guest() {
  let dir = tempfile::tempdir().unwrap();
  let exe = write(dir.path(), "needy", &elf(elf::EM_X86_64, None, 0));

  assert_eq!(find(None, &exe, "x86_64").unwrap(), exe);
}

#[test]
fn without_a_guest_needy_the_error_lists_where_it_looked() {
  let dir = tempfile::tempdir().unwrap();
  let exe = write(dir.path(), "needy", b"#!/bin/sh\n");

  let error = find(None, &exe, "aarch64").unwrap_err().to_string();

  let beside = dir.path().join("needy-linux-aarch64");
  assert!(error.starts_with(&format!("no guest needy for aarch64 actors: {}: does not exist", beside.display())), "{error}");
  if cfg!(target_os = "linux") {
    assert!(error.contains(&format!("; {}: is not a 64-bit ELF executable", exe.display())), "{error}");
  }
  assert!(error.ends_with("or pass --guest-needy PATH"), "{error}");
}

#[test]
fn the_guest_disk_is_squashfs_with_needy_at_its_root_and_is_made_once_per_binary() {
  use std::io::Read as _;
  let dir = tempfile::tempdir().unwrap();
  let binary = write(dir.path(), "needy-linux-x86_64", &elf(elf::EM_X86_64, None, 0));
  let cache = dir.path().join("guest");

  let made = disk(&binary, &cache).unwrap();

  let bytes = std::fs::read(&made).unwrap();
  assert_eq!(&bytes[..4], b"hsqs", "SquashFS's magic, little-endian");
  assert_eq!(bytes.len() % 4096, 0, "padded to 4 KiB, a whole number of disk sectors");
  // Source: https://github.com/torvalds/linux/blob/v6.18/fs/squashfs/squashfs_fs.h
  // (`struct squashfs_super_block`: `compression` follows five 32-bit
  // fields; `ZLIB_COMPRESSION` is 1), which both actor kernels read.
  assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 1, "zlib");
  let fs = backhand::FilesystemReader::from_reader(std::io::Cursor::new(bytes)).unwrap();
  let files: Vec<_> = fs.files().filter(|node| matches!(node.inner, backhand::InnerNode::File(_))).collect();
  assert_eq!(files.len(), 1);
  assert_eq!(files[0].fullpath, Path::new("/needy"));
  assert_eq!((files[0].header.permissions, files[0].header.uid, files[0].header.gid), (0o755, 0, 0));
  let backhand::InnerNode::File(file) = &files[0].inner else {
    unreachable!()
  };
  let mut content = Vec::new();
  fs.file(file).reader().read_to_end(&mut content).unwrap();
  assert_eq!(content, std::fs::read(&binary).unwrap());
  // The same binary, the same disk; another binary, another disk.
  assert_eq!(disk(&binary, &cache).unwrap(), made);
  let other = write(dir.path(), "other", &elf(elf::EM_AARCH64, None, 0));
  assert_ne!(disk(&other, &cache).unwrap(), made);
}
