//! Tar archives for copy-in and mirror-out mappings.

use std::path::{Component, Path};

use anyhow::{Result, bail};
use needy_config::filter_allows;

/// Packs `root` (a file or directory) with rclone-style `filter` rules
/// applied to paths relative to `root`.
pub fn pack(root: &Path, filter: &[String]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    if root.is_file() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        builder.append_path_with_name(root, name)?;
        return Ok(builder.into_inner()?);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let kind = entry.file_type()?;
            let probe = if kind.is_dir() {
                format!("{relative}/")
            } else {
                relative.clone()
            };
            if !filter_allows(filter, &probe) {
                continue;
            }
            if kind.is_dir() {
                builder.append_dir(&relative, &path)?;
                stack.push(path);
            } else if kind.is_file() || kind.is_symlink() {
                builder.append_path_with_name(&path, &relative)?;
            }
        }
    }
    Ok(builder.into_inner()?)
}

/// Unpacks into `destination`, refusing paths that would escape it.
pub fn unpack(archive: &[u8], destination: &Path) -> Result<usize> {
    std::fs::create_dir_all(destination)?;
    let mut tar = tar::Archive::new(std::io::Cursor::new(archive));
    tar.set_preserve_permissions(true);
    let mut count = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            bail!("unsafe path in archive: {}", path.display());
        }
        entry.unpack_in(destination)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_round_trips() {
        let src = std::env::temp_dir().join(format!("needy-pack-{}", std::process::id()));
        let dst = src.with_extension("out");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
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
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }
}
