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
/// absolute paths or `..` components are skipped by `tar` itself. `filter`
/// rules apply as [`pack`] applies them, to paths relative to the archive's
/// root (a leading `./`, as `tar -C DIR .` writes, is not part of them): an
/// entry is skipped when a rule excludes it or a directory above it.
pub fn unpack(archive: &[u8], destination: &Path, filter: &[String]) -> Result<usize> {
  std::fs::create_dir_all(destination)?;
  let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
  tar.set_preserve_permissions(true);
  let mut count = 0;
  for entry in tar.entries()? {
    let mut entry = entry?;
    if !filter.is_empty() && !allowed(filter, &entry.path()?, entry.header().entry_type().is_dir()) {
      continue;
    }
    if entry.unpack_in(destination)? {
      count += 1;
    }
  }
  Ok(count)
}

/// Whether `filter` lets the archive entry at `path` through: neither the
/// entry nor a directory above it is excluded, as `pack`'s walk skips an
/// excluded directory with its contents.
fn allowed(filter: &[String], path: &Path, is_dir: bool) -> bool {
  let parts: Vec<String> = path
    .components()
    .filter_map(|c| match c {
      std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
      _ => None,
    })
    .collect();
  let mut relative = String::new();
  for (i, part) in parts.iter().enumerate() {
    if !relative.is_empty() {
      relative.push('/');
    }
    relative.push_str(part);
    let last = i + 1 == parts.len();
    let probe = if !last || is_dir {
      format!("{relative}/")
    } else {
      relative.clone()
    };
    if !filter_allows(filter, &probe) {
      return false;
    }
  }
  true
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
    unpack(&archive, &dst, &[]).unwrap();
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
    assert_eq!(unpack(&archive, &dst, &[]).unwrap(), 1);
    assert!(dst.join("ok.txt").exists());
    assert!(!dir.path().join("escape.txt").exists());
  }

  /// An archive as `tar -c -C DIR .` writes it: every name starts with
  /// `./`, and directories come before their contents.
  fn tar_dot(entries: &[(&str, Option<&str>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
      let mut header = tar::Header::new_gnu();
      match data {
        Some(data) => {
          header.set_entry_type(tar::EntryType::Regular);
          header.set_size(data.len() as u64);
          header.set_mode(0o644);
          header.set_cksum();
          builder.append_data(&mut header, name, data.as_bytes()).unwrap();
        }
        None => {
          header.set_entry_type(tar::EntryType::Directory);
          header.set_size(0);
          header.set_mode(0o755);
          header.set_cksum();
          builder.append_data(&mut header, name, &[][..]).unwrap();
        }
      }
    }
    builder.into_inner().unwrap()
  }

  #[test]
  fn unpack_applies_the_filter_as_pack_does() {
    let archive = tar_dot(&[
      ("./", None),
      ("./keep.txt", Some("k")),
      ("./auth.json", Some("s")),
      ("./cache/", None),
      ("./cache/blob", Some("x")),
      ("./nested/", None),
      ("./nested/auth.json", Some("n")),
      ("./nested/debug.log", Some("x")),
    ]);
    let filter: Vec<String> = ["- /auth.json", "- cache/", "- *.log", "+ **"].iter().map(|s| s.to_string()).collect();
    let dir = tempfile::tempdir().unwrap();

    unpack(&archive, dir.path(), &filter).unwrap();

    assert!(dir.path().join("keep.txt").exists());
    assert!(!dir.path().join("auth.json").exists(), "an anchored rule matches at the root");
    assert!(dir.path().join("nested/auth.json").exists(), "an anchored rule matches only at the root");
    assert!(!dir.path().join("cache").exists(), "an excluded directory is skipped with its contents");
    assert!(!dir.path().join("nested/debug.log").exists());
  }

  #[test]
  fn unpack_skips_what_is_under_an_excluded_directory_without_its_entry() {
    // `- /cache/**` matches the directory's contents; an archive that has
    // only the contents, no entry for the directory, is filtered the same.
    let archive = tar_dot(&[("./cache/deep/blob", Some("x")), ("./keep.txt", Some("k"))]);
    let filter: Vec<String> = vec!["- /cache/".into(), "+ **".into()];
    let dir = tempfile::tempdir().unwrap();

    assert_eq!(unpack(&archive, dir.path(), &filter).unwrap(), 1);

    assert!(!dir.path().join("cache").exists());
    assert!(dir.path().join("keep.txt").exists());
  }
}
