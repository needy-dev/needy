use anyhow::{Context, Result, ensure};
use std::io::{Cursor, Read};
use std::path::{Component, Path};

pub const MAX: usize = 32 << 20;
const ENTRIES: usize = 10000;

pub fn validate(bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= MAX, "workspace archive exceeds 32 MiB");
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < ENTRIES, "too many workspace entries");
        let entry = entry?;
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "workspace links and special files are unsupported"
        );
        let path = entry.path()?;
        ensure!(
            !path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
            "unsafe workspace path"
        );
    }
    Ok(())
}

pub fn pack(directory: &Path) -> Result<Vec<u8>> {
    ensure!(directory.is_dir(), "workspace source must be a directory");
    let mut builder = tar::Builder::new(Vec::new());
    let mut count = 0;
    fn visit(
        builder: &mut tar::Builder<Vec<u8>>,
        root: &Path,
        directory: &Path,
        count: &mut usize,
    ) -> Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | ".needy" | ".env" | ".env.local" | "target")
            ) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            *count += 1;
            ensure!(*count <= ENTRIES, "too many workspace entries");
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            if kind.is_dir() {
                builder.append_dir(relative, &path)?;
                visit(builder, root, &path, count)?;
            } else if kind.is_file() {
                builder.append_path_with_name(&path, relative)?;
            }
            ensure!(
                builder.get_ref().len() <= MAX,
                "workspace archive exceeds 32 MiB"
            );
        }
        Ok(())
    }
    visit(&mut builder, directory, directory, &mut count)?;
    let bytes = builder.into_inner()?;
    validate(&bytes)?;
    Ok(bytes)
}

pub fn unpack(bytes: &[u8], directory: &Path) -> Result<()> {
    validate(bytes)?;
    if directory.exists() {
        ensure!(
            std::fs::read_dir(directory)?.next().is_none(),
            "download destination must be empty"
        );
    }
    std::fs::create_dir_all(directory)?;
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    for entry in archive.entries()? {
        let mut entry = entry?;
        ensure!(entry.unpack_in(directory)?, "unsafe workspace path");
    }
    Ok(())
}

pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(path).context("cannot read private configuration")?;
    ensure!(
        file.metadata()?.permissions().mode() & 0o077 == 0,
        "configuration must be private (mode 0600)"
    );
    let mut bytes = Vec::new();
    file.take((4 << 20) + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 << 20,
        "private configuration exceeds 4 MiB"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_round_trip_excludes_local_configuration() {
        let input = tempfile::tempdir().unwrap();
        std::fs::write(input.path().join("hello.txt"), b"exact artifact\n").unwrap();
        std::fs::write(input.path().join(".env.local"), b"do not transfer").unwrap();
        let output = tempfile::tempdir().unwrap();
        unpack(&pack(input.path()).unwrap(), output.path()).unwrap();
        assert_eq!(
            std::fs::read(output.path().join("hello.txt")).unwrap(),
            b"exact artifact\n"
        );
        assert!(!output.path().join(".env.local").exists());
    }
}
