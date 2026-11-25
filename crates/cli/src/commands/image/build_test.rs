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
    let compiled = needy_core::recipe::compile(&recipe, &needy_core::recipe::HostInfo::default(), &needy_core::recipe::PerRun::at(0, 0));
    assert!(compiled.kernel_build_env.contains_key("KERNELRELEASE"), "{name}");
    for key in compiled.kernel_build_env.keys() {
      assert!(args.contains(key), "{name}: {key} is not an ARG of the kernel stage: {args:?}");
    }
  }
}

/// A root file system in `dir` for the shim script: the proxy, merged
/// `/usr` with `/bin` a link to `usr/bin`, and the files `files` (path,
/// content, mode) and links `links` (path, target) under it. Returns its
/// physical path.
fn image_root(dir: &Path, files: &[(&str, &str, u32)], links: &[(&str, &str)]) -> PathBuf {
  let root = std::fs::canonicalize(dir).unwrap();
  for (path, content, mode) in [(".needy/proxy", "proxy", 0o755)].iter().chain(files) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).unwrap();
  }
  std::fs::create_dir_all(root.join("usr/bin")).unwrap();
  std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap();
  for (path, target) in links {
    std::os::unix::fs::symlink(target, root.join(path)).unwrap();
  }
  root
}

/// Runs `images/container/shim.sh` on `root` with the PATH directories
/// `/usr/local/bin`, `/usr/bin` and `/bin`, and its default list of
/// programs to shim in place.
fn shim(root: &Path) {
  run_shim(root, None);
}

/// [`shim`] with `names` (space-separated) as the programs to shim in place
/// (`NEEDY_SHIM_NAMES`).
fn shim_listed(root: &Path, names: &str) {
  run_shim(root, Some(names));
}

fn run_shim(root: &Path, names: Option<&str>) {
  let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/container/shim.sh");
  let mut command = std::process::Command::new("sh");
  command.arg(script).arg(root).args(["/.needy/proxy", "/usr/local/bin", "/usr/bin", "/bin"]).env_remove("NEEDY_SHIM_NAMES");
  if let Some(names) = names {
    command.env("NEEDY_SHIM_NAMES", names);
  }
  let status = command.status().unwrap();
  assert!(status.success(), "shim.sh: {status}");
}

fn same_file(a: &Path, b: &Path) -> bool {
  let (a, b) = (std::fs::metadata(a).unwrap(), std::fs::metadata(b).unwrap());
  (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[test]
fn the_shim_stage_moves_each_program_to_needy_real_and_links_the_proxy_in_its_place() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/bin/tool", "#!/bin/sh\necho tool\n", 0o755)], &[]);

  shim_listed(&root, "tool");

  let real = root.join(".needy/real/usr/bin/tool");
  assert_eq!(std::fs::read_to_string(&real).unwrap(), "#!/bin/sh\necho tool\n");
  assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
  assert!(same_file(&root.join("usr/bin/tool"), &root.join(".needy/proxy")), "usr/bin/tool is not the proxy");
}

#[test]
fn the_shim_stage_shims_a_directory_reached_through_a_link_once() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/bin/tool", "tool", 0o755)], &[]);

  shim_listed(&root, "tool");

  // `/bin` is `usr/bin`: no `/.needy/real/bin`, and the proxy is not copied
  // as a real program on a second pass.
  assert!(!root.join(".needy/real/bin").exists());
  assert_eq!(std::fs::read_to_string(root.join(".needy/real/usr/bin/tool")).unwrap(), "tool");
}

#[test]
fn the_shim_stage_leaves_files_that_are_not_programs() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/bin/notes", "data", 0o644)], &[]);

  shim(&root);

  assert_eq!(std::fs::read_to_string(root.join("usr/bin/notes")).unwrap(), "data");
  assert!(!root.join(".needy/real/usr/bin/notes").exists());
}

#[test]
fn a_link_to_a_shimmed_program_stays_a_link() {
  // `python3` -> `python3.12`: the link ends at a shim, which finds the
  // real `python3.12` and keeps the name `python3` the caller used.
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/bin/python3.12", "py", 0o755)], &[("usr/bin/python3", "python3.12")]);

  shim_listed(&root, "python3 python3.12");

  assert_eq!(std::fs::read_link(root.join("usr/bin/python3")).unwrap(), Path::new("python3.12"));
  assert!(!root.join(".needy/real/usr/bin/python3").exists());
}

#[test]
fn a_link_to_a_program_elsewhere_becomes_a_shim_whose_real_program_is_that_link() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/lib/tool/bin/tool", "tool", 0o755)], &[("usr/bin/tool", "../lib/tool/bin/tool")]);

  shim_listed(&root, "tool");

  assert!(same_file(&root.join("usr/bin/tool"), &root.join(".needy/proxy")), "usr/bin/tool is not the proxy");
  assert_eq!(std::fs::read_link(root.join(".needy/real/usr/bin/tool")).unwrap(), Path::new("/usr/lib/tool/bin/tool"));
}

// ROOT CAUSE:
//
// If an image `FROM` the base ran the shim step again to shim the programs
// it added, every program that was a shim already was copied to
// `/.needy/real` as its own real program, because the step took any
// executable for a real one.
//
// Before the fix, a second run replaced the real programs with the proxy.
// The fix leaves a shim as it is, so the step may run any number of times.
#[test]
fn a_second_run_shims_the_added_programs_and_keeps_the_real_ones() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/bin/tool", "tool", 0o755)], &[("usr/bin/tool-link", "tool")]);
  shim_listed(&root, "tool tool-link jq");
  std::fs::create_dir_all(root.join("usr/local/bin")).unwrap();
  std::fs::write(root.join("usr/local/bin/jq"), "jq").unwrap();
  std::fs::set_permissions(root.join("usr/local/bin/jq"), std::fs::Permissions::from_mode(0o755)).unwrap();

  shim_listed(&root, "tool tool-link jq");

  assert_eq!(std::fs::read_to_string(root.join(".needy/real/usr/bin/tool")).unwrap(), "tool");
  assert_eq!(std::fs::read_to_string(root.join(".needy/real/usr/local/bin/jq")).unwrap(), "jq");
  assert!(same_file(&root.join("usr/local/bin/jq"), &root.join(".needy/proxy")), "usr/local/bin/jq is not the proxy");
  assert_eq!(std::fs::read_link(root.join("usr/bin/tool-link")).unwrap(), Path::new("tool"));
}

#[test]
fn a_program_that_replaced_a_shim_becomes_the_real_one() {
  // A package upgrade in an image `FROM` the base writes a new file over a
  // shim.
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[("usr/lib/tool", "old", 0o755)], &[("usr/bin/tool", "../lib/tool")]);
  shim_listed(&root, "tool");
  std::fs::remove_file(root.join("usr/bin/tool")).unwrap();
  std::fs::write(root.join("usr/bin/tool"), "new").unwrap();
  std::fs::set_permissions(root.join("usr/bin/tool"), std::fs::Permissions::from_mode(0o755)).unwrap();

  shim_listed(&root, "tool");

  let real = root.join(".needy/real/usr/bin/tool");
  assert!(!real.is_symlink(), "the old real program, a link, was written through");
  assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
  assert_eq!(std::fs::read_to_string(root.join("usr/lib/tool")).unwrap(), "old");
  assert!(same_file(&root.join("usr/bin/tool"), &root.join(".needy/proxy")), "usr/bin/tool is not the proxy");
}

// ROOT CAUSE:
//
// If a container actor image's programs were all shims, a program that finds
// its files from its own path, or a tool that reads a program file, broke,
// because every real program ran from `/.needy/real` and every file on
// `PATH` was the shim stub: Node.js took `/.needy/real/usr/local` for npm's
// global prefix, and npm read `/usr/bin/ldd` as text to tell glibc from musl
// and found neither (seen 2026-11-16 installing Claude Code in an image
// `FROM` `needy-agents`).
//
// Before the fix, the shim step shimmed every program on `PATH` in place.
// The fix shims only the shells in place; the guest needy reaches every other
// program through a trampoline on `PATH`, and it runs at its own path.
#[test]
fn the_shim_stage_shims_only_the_shells_in_place_by_default() {
  let dir = tempfile::tempdir().unwrap();
  let ldd = "#!/bin/bash\n# GNU C Library ldd\n";
  let root = image_root(
    dir.path(),
    &[
      ("usr/local/bin/node", "node", 0o755),
      ("usr/bin/ldd", ldd, 0o755),
      ("usr/bin/bash", "bash", 0o755),
      ("usr/bin/dash", "dash", 0o755),
    ],
    &[("usr/bin/sh", "dash")],
  );

  shim(&root);

  assert_eq!(std::fs::read_to_string(root.join("usr/local/bin/node")).unwrap(), "node");
  assert_eq!(std::fs::read_to_string(root.join("usr/bin/ldd")).unwrap(), ldd);
  assert!(!root.join(".needy/real/usr/local/bin/node").exists());
  assert!(!root.join(".needy/real/usr/bin/ldd").exists());
  assert!(same_file(&root.join("usr/bin/bash"), &root.join(".needy/proxy")), "usr/bin/bash is not the proxy");
  assert!(same_file(&root.join("usr/bin/dash"), &root.join(".needy/proxy")), "usr/bin/dash is not the proxy");
  assert_eq!(std::fs::read_link(root.join("usr/bin/sh")).unwrap(), Path::new("dash"));
}

// An image built with an earlier shim step has every program in place; a
// later step with the default list must put the others back.
#[test]
fn a_shim_whose_name_is_no_longer_listed_gets_its_real_program_back() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(
    dir.path(),
    &[
      ("usr/bin/tool", "tool", 0o755),
      ("usr/bin/bash", "bash", 0o755),
      ("usr/lib/other/bin/other", "other", 0o755),
    ],
    &[("usr/bin/other", "../lib/other/bin/other")],
  );
  shim_listed(&root, "tool bash other");

  shim(&root);

  assert_eq!(std::fs::read_to_string(root.join("usr/bin/tool")).unwrap(), "tool");
  assert!(!same_file(&root.join("usr/bin/tool"), &root.join(".needy/proxy")), "usr/bin/tool is still the proxy");
  assert!(!root.join(".needy/real/usr/bin/tool").exists());
  assert_eq!(std::fs::read_link(root.join("usr/bin/other")).unwrap(), Path::new("/usr/lib/other/bin/other"));
  assert!(!root.join(".needy/real/usr/bin/other").exists());
  assert!(same_file(&root.join("usr/bin/bash"), &root.join(".needy/proxy")), "usr/bin/bash is not the proxy");
}

#[test]
fn a_dangling_link_is_left_alone() {
  let dir = tempfile::tempdir().unwrap();
  let root = image_root(dir.path(), &[], &[("usr/bin/gone", "nowhere")]);

  shim(&root);

  assert_eq!(std::fs::read_link(root.join("usr/bin/gone")).unwrap(), Path::new("nowhere"));
}

/// The `RUN` instructions of `images/Dockerfile`, each with its continued
/// lines joined.
fn run_instructions() -> Vec<String> {
  let dockerfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/Dockerfile");
  let text = std::fs::read_to_string(dockerfile).unwrap();
  let mut runs = Vec::new();
  let mut current: Option<String> = None;
  for line in text.lines() {
    let line = line.trim();
    if line.starts_with('#') {
      continue;
    }
    if let Some(run) = current.as_mut() {
      run.push(' ');
      run.push_str(line.trim_end_matches('\\'));
    } else if line.starts_with("RUN ") {
      current = Some(line.trim_end_matches('\\').to_string());
    }
    if !line.ends_with('\\')
      && let Some(run) = current.take()
    {
      runs.push(run);
    }
  }
  runs
}

// ROOT CAUSE:
//
// If two source trees were built on one host, the second could ship the
// first's crates, because the BuildKit cache mount on /src/target is shared
// by every build and cargo trusts mtimes: files unpacked from an archive of a
// commit carry the commit's date, older than the artifacts already cached.
//
// Before the fix, `cargo build` ran on the copied tree as it was.
// The fix touches every source file before cargo runs, so each workspace
// crate is newer than any cached artifact and is rebuilt from this tree.
#[test]
fn every_cargo_build_on_a_shared_target_cache_touches_the_sources_first() {
  let runs: Vec<String> = run_instructions().into_iter().filter(|run| run.contains("target=/src/target")).collect();
  assert!(!runs.is_empty(), "no RUN mounts a cache on /src/target");
  for run in runs {
    let touch = run.find("-exec touch").unwrap_or_else(|| panic!("the sources are not touched: {run}"));
    let cargo = run.find("cargo build").unwrap_or_else(|| panic!("no cargo build: {run}"));
    assert!(touch < cargo, "the sources are touched after cargo builds: {run}");
  }
}

// The VM image's manifest records the `actor-base` its root file system was
// derived from (B1 and B3 of
// docs/ai/references/architecture/2026-11-25-layered-oci-base-spec.md),
// beside the base ABI the build exported.
#[test]
fn a_built_vm_image_records_its_base_and_folds_in_the_base_abi() {
  let dir = tempfile::tempdir().unwrap();
  std::fs::write(dir.path().join(needy_core_actor::image::base::FILE), "1\n").unwrap();
  let image = bollard::models::ImageInspect {
    root_fs: Some(bollard::models::ImageInspectRootFs {
      typ: "layers".into(),
      layers: Some(vec!["sha256:01".into(), "sha256:02".into()]),
    }),
    ..Default::default()
  };

  record_build(dir.path(), None, base_of(BUILT_BASE_IMAGE, image).unwrap()).unwrap();

  let manifest = manifest::read(dir.path()).unwrap().unwrap();
  assert_eq!(manifest.base_abi, 1);
  assert_eq!(
    manifest.base,
    Some(manifest::Base {
      image: "needy-actor-base:latest".into(),
      digest: None,
      diff_ids: vec!["sha256:01".into(), "sha256:02".into()],
    })
  );
  assert!(!dir.path().join(needy_core_actor::image::base::FILE).exists());
}

#[test]
fn a_base_the_engine_lists_no_layers_of_is_refused() {
  let error = base_of(BUILT_BASE_IMAGE, bollard::models::ImageInspect::default()).unwrap_err();

  assert_eq!(error.to_string(), "the engine lists no layers of needy-actor-base:latest");
}

// `needy image build` builds these targets of `images/Dockerfile` by name;
// a renamed stage would fail only when someone builds it.
#[test]
fn every_target_needy_image_build_names_is_a_stage_of_the_dockerfile() {
  let dockerfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/Dockerfile");
  let text = std::fs::read_to_string(dockerfile).unwrap();
  let stages: Vec<&str> =
    text.lines().filter(|line| line.starts_with("FROM ")).filter_map(|line| line.rsplit_once(" AS ").map(|(_, stage)| stage)).collect();

  for target in [
    "actor",
    "actor-macos-vz",
    "actor-base",
    "actor-container",
    "executor",
  ] {
    assert!(stages.contains(&target), "no stage {target} in {stages:?}");
  }
}
