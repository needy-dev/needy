//! The guest needy: the `needy` that runs inside an actor as its init,
//! proxy and shim handler. The daemon brings its own to every actor it
//! starts, at [`crate::contract::GUEST`]: a bind mount for the container
//! drivers, the guest disk ([`disk`]) for the VM drivers. It is a static
//! Linux build of the daemon's own version, for the actor's architecture.
//!
//! See `docs/ai/references/architecture/2026-11-12-actor-images-design.md`.

use std::path::{Path, PathBuf};

use object::elf;
use object::read::elf::{Dyn, FileHeader, ProgramHeader};

/// What [`find`] and [`check`] refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// No usable guest needy where the daemon looks: each place it tried and
  /// why it was not used.
  #[error(
    "no guest needy for {arch} actors: {}; build one with `make guest` in a needy checkout (it writes bin/needy-linux-{arch}) and put it next to needy, or pass --guest-needy PATH",
    tried.iter().map(|(path, why)| format!("{}: {why}", path.display())).collect::<Vec<_>>().join("; ")
  )]
  NotFound {
    arch: String,
    tried: Vec<(PathBuf, String)>,
  },
  /// The guest needy that was named is not a static Linux build for the
  /// actors' architecture.
  #[error("the guest needy {}: {why}", path.display())]
  Refused { path: PathBuf, why: String },
  /// The guest disk could not be written.
  #[error("cannot write the guest disk {}: {message}", path.display())]
  Disk { path: PathBuf, message: String },
}

/// The architecture of this machine's actors, as Rust and `uname -m` name
/// it.
// NOTICE: every driver runs guests of the host's architecture: Firecracker
// and the engine on Linux run the host's, and Virtualization.framework and
// Apple's `container` run arm64 on Apple silicon, the only Macs they
// support. A driver that runs another architecture would name its own.
pub fn arch() -> &'static str {
  std::env::consts::ARCH
}

/// The file name of a guest needy for `arch` next to a needy executable:
/// how the macOS release archive and `make guest` ship it.
pub fn file_name(arch: &str) -> String {
  format!("needy-linux-{arch}")
}

/// The guest needy for `arch` actors: `given` (`--guest-needy`) when there is
/// one, which must then be usable; else `needy-linux-<arch>` next to `exe`
/// (the daemon's own executable); else, on Linux, `exe` itself when it is a
/// static build for `arch`, as the release binaries are.
pub fn find(given: Option<&Path>, exe: &Path, arch: &str) -> Result<PathBuf, Error> {
  if let Some(path) = given {
    check(path, arch)?;
    return Ok(path.to_path_buf());
  }
  let mut tried = Vec::new();
  let mut candidates = Vec::new();
  if let Some(dir) = exe.parent() {
    candidates.push(dir.join(file_name(arch)));
  }
  if cfg!(target_os = "linux") {
    candidates.push(exe.to_path_buf());
  }
  for path in candidates {
    match check(&path, arch) {
      Ok(()) => return Ok(path),
      Err(Error::Refused { why, .. }) => tried.push((path, why)),
      Err(e) => tried.push((path, e.to_string())),
    }
  }
  Err(Error::NotFound {
    arch: arch.to_string(),
    tried,
  })
}

/// Fails unless `path` is a static Linux executable for `arch`: an ELF file
/// for Linux (System V or GNU OS/ABI) of `arch`'s machine, with no program
/// interpreter and no shared libraries.
pub fn check(path: &Path, arch: &str) -> Result<(), Error> {
  let refused = |why: String| Error::Refused {
    path: path.to_path_buf(),
    why,
  };
  let file = match std::fs::File::open(path) {
    Ok(file) => file,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(refused("does not exist".into())),
    Err(e) => return Err(refused(format!("cannot be read: {e}"))),
  };
  // Reads only the headers and the segments it asks for, not the whole
  // binary.
  let data = object::ReadCache::new(file);
  let data = &data;
  let header = elf::FileHeader64::<object::Endianness>::parse(data)
    .map_err(|_| refused("is not a 64-bit ELF executable; the guest needy is a static Linux build of needy".into()))?;
  let endian = header.endian().map_err(|e| refused(format!("has an unreadable ELF header: {e}")))?;
  // Reference: https://refspecs.linuxfoundation.org/elf/gabi4+/ch4.eheader.html
  // (`EI_OSABI`, `e_machine`); Linux binaries carry `ELFOSABI_NONE` or
  // `ELFOSABI_GNU`, whose values `object::elf` holds.
  let os_abi = header.e_ident().os_abi;
  if os_abi != elf::ELFOSABI_NONE && os_abi != elf::ELFOSABI_GNU {
    return Err(refused(format!("is an ELF executable for another operating system (OS/ABI {})", os_abi.0)));
  }
  let machine = header.e_machine(endian);
  let found = match machine {
    elf::EM_X86_64 => "x86_64",
    elf::EM_AARCH64 => "aarch64",
    _ => "",
  };
  if found != arch {
    let found = if found.is_empty() {
      format!("ELF machine {}", machine.0)
    } else {
      found.to_string()
    };
    return Err(refused(format!("is built for {found}, and this machine's actors are {arch}")));
  }
  let segments = header.program_headers(endian, data).map_err(|e| refused(format!("has unreadable program headers: {e}")))?;
  for segment in segments {
    if let Ok(Some(interpreter)) = segment.interpreter(endian, data) {
      return Err(refused(format!(
        "is dynamically linked (its program interpreter is {}); the guest needy must be static, as a musl build is",
        String::from_utf8_lossy(interpreter)
      )));
    }
    let needed = match segment.dynamic(endian, data) {
      Ok(Some(entries)) => entries.iter().filter(|entry| entry.d_tag(endian) == elf::DT_NEEDED).count(),
      _ => 0,
    };
    if needed > 0 {
      return Err(refused(format!("needs {needed} shared libraries; the guest needy must be static, as a musl build is")));
    }
  }
  Ok(())
}

/// The guest disk of the guest needy `binary`, which the VM drivers attach
/// as the actor's second virtio-blk device (`/dev/vdb`): a SquashFS image,
/// zlib-compressed, with `binary` as `needy` at its root, mode 0755, owned
/// by root, as the boot stub expects (`images/stub/boot.c`).
///
/// It is built once per binary, named by its SHA-256, in `cache`, and
/// written beside and renamed, so actors starting together share it. It
/// reads and compresses the whole binary: call it off the async runtime.
// NOTICE: SquashFS rather than ext4: it is read-only by design, both actor
// kernels read it (`CONFIG_SQUASHFS`, `CONFIG_SQUASHFS_ZLIB` in
// `images/kernel/firecracker-6.18.config` and `containerization-arm64.config`),
// and `backhand` writes it in Rust on Linux and macOS alike, where no
// `mke2fs` is at hand. The times are zero, so one binary always makes the
// same disk.
// NOTICE: the daemon never removes the disks of guest binaries no longer in
// use; `needy image rm --guest-disks` removes every one but the current
// guest's ([`disk_name`]).
pub fn disk(binary: &Path, cache: &Path) -> Result<PathBuf, Error> {
  use sha2::Digest as _;
  let bytes = std::fs::read(binary).map_err(|e| Error::Refused {
    path: binary.to_path_buf(),
    why: format!("cannot be read: {e}"),
  })?;
  let path = cache.join(disk_name(&sha2::Sha256::digest(&bytes)));
  if path.exists() {
    return Ok(path);
  }
  let failed = |message: String| Error::Disk {
    path: path.clone(),
    message,
  };
  std::fs::create_dir_all(cache).map_err(|e| failed(format!("cannot create {}: {e}", cache.display())))?;
  let mut staged = tempfile::NamedTempFile::new_in(cache).map_err(|e| failed(e.to_string()))?;
  let mut fs = backhand::FilesystemWriter::default();
  let gzip = backhand::FilesystemCompressor::new(backhand::compression::Compressor::Gzip, None).map_err(|e| failed(e.to_string()))?;
  fs.set_compressor(gzip);
  fs.set_root_mode(0o755);
  fs.push_file(std::io::Cursor::new(bytes), "needy", backhand::NodeHeader::new(0o755, 0, 0, 0)).map_err(|e| failed(e.to_string()))?;
  fs.write(staged.as_file_mut()).map_err(|e| failed(e.to_string()))?;
  staged.persist(&path).map_err(|e| failed(e.to_string()))?;
  Ok(path)
}

/// The file name of the guest disk [`disk`] makes of a binary whose SHA-256
/// is `digest`: its first 16 hex digits, with `.squashfs`.
pub fn disk_name(digest: &[u8]) -> String {
  use std::fmt::Write as _;
  digest[..8].iter().fold(String::new(), |mut hex, byte| {
    // NOTICE: writing to a `String` cannot fail.
    let _ = write!(hex, "{byte:02x}");
    hex
  }) + ".squashfs"
}

#[cfg(test)]
#[path = "guest_test.rs"]
mod tests;
