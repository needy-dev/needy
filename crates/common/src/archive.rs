//! Tar archives for file mappings: pack a file or directory with filter rules,
//! and unpack into a directory.

use std::path::Path;

use anyhow::Result;

use crate::filter::filter_allows;

/// Packs `root` (a file or directory). `filter` rules apply to paths relative
/// to `root`; a directory that a rule excludes is skipped with its contents.
pub fn pack(root: &Path, filter: &[String]) -> Result<Vec<u8>> {
  let mut builder = tar::Builder::new(Vec::new());
  builder.follow_symlinks(false);
  if root.is_file() {
    let name = root.file_name().unwrap_or_default();
    builder.append_path_with_name(root, name)?;
    return Ok(builder.into_inner()?);
  }
  let walker = walkdir::WalkDir::new(root).min_depth(1).sort_by_file_name().into_iter().filter_entry(|entry| {
    let relative = relative(root, entry.path());
    let probe = if entry.file_type().is_dir() {
      format!("{relative}/")
    } else {
      relative
    };
    filter_allows(filter, &probe)
  });
  for entry in walker {
    let entry = entry?;
    let relative = relative(root, entry.path());
    if entry.file_type().is_dir() {
      builder.append_dir(&relative, entry.path())?;
    } else {
      builder.append_path_with_name(entry.path(), &relative)?;
    }
  }
  Ok(builder.into_inner()?)
}

fn relative(root: &Path, path: &Path) -> String {
  path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Unpacks into `destination` and returns the number of entries. Entries with
/// absolute paths or `..` components are skipped by `tar` itself.
pub fn unpack(archive: &[u8], destination: &Path) -> Result<usize> {
  std::fs::create_dir_all(destination)?;
  let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
  tar.set_preserve_permissions(true);
  let mut count = 0;
  for entry in tar.entries()? {
    if entry?.unpack_in(destination)? {
      count += 1;
    }
  }
  Ok(count)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn filters_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("out");
    std::fs::create_dir_all(src.join("sessions")).unwrap();
    std::fs::write(src.join("config.toml"), "a").unwrap();
    std::fs::write(src.join("auth.json"), "secret").unwrap();
    std::fs::write(src.join("sessions/log.jsonl"), "x").unwrap();
    let filter = vec![
      "- /auth.json".to_string(),
      "- /sessions/**".to_string(),
      "+ **".to_string(),
    ];
    let archive = pack(&src, &filter).unwrap();
    unpack(&archive, &dst).unwrap();
    assert!(dst.join("config.toml").exists());
    assert!(!dst.join("auth.json").exists());
    assert!(!dst.join("sessions/log.jsonl").exists());
  }

  #[test]
  fn unpack_skips_escaping_paths() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header.clone(), "ok.txt", &b"x"[..]).unwrap();
    // `append_data` refuses `..`, so write the name into the header directly.
    let name = b"../escape.txt";
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();
    builder.append(&header, &b"x"[..]).unwrap();
    let archive = builder.into_inner().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("out");
    assert_eq!(unpack(&archive, &dst).unwrap(), 1);
    assert!(dst.join("ok.txt").exists());
    assert!(!dir.path().join("escape.txt").exists());
  }
}
