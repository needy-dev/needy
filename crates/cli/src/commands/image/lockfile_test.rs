use clap::Parser;

use super::*;
use crate::commands::image;

#[derive(Parser)]
struct Harness {
  #[command(subcommand)]
  cmd: Top,
}

#[derive(clap::Subcommand)]
enum Top {
  Image(image::Args),
}

fn digest(ch: char) -> String {
  format!("sha256:{}", ch.to_string().repeat(64))
}

#[test]
fn lockfile_round_trip_and_removal_when_empty() {
  let dir = tempfile::tempdir().unwrap();
  let image_file = dir.path().join("agent.yaml");
  std::fs::write(&image_file, "root: example.com/root:1\n").unwrap();
  let lock_path = path_for(&image_file);
  assert_eq!(lock_path, dir.path().join("agent.yaml.lock"));

  // Missing lockfile reads as empty default.
  assert_eq!(read(&lock_path).unwrap(), Lockfile::default());

  let lock = Lockfile {
    commit: Some(LockCommit {
      layer: digest('a'),
      base: digest('b'),
    }),
  };
  write(&lock_path, &lock).unwrap();
  assert!(lock_path.exists());
  assert_eq!(read(&lock_path).unwrap(), lock);
  // User's image file is never modified.
  assert_eq!(std::fs::read_to_string(&image_file).unwrap(), "root: example.com/root:1\n");

  // Writing an empty lockfile removes the file.
  write(&lock_path, &Lockfile::default()).unwrap();
  assert!(!lock_path.exists());
}

#[test]
fn format_run_snippet_emits_valid_yaml_layer_with_cwd_and_quoting() {
  assert_eq!(format_run_snippet(&[]), None);

  let commands = vec![
    RecordedCommand {
      argv: vec!["apt-get".into(), "update".into()],
      cwd: "/".into(),
    },
    RecordedCommand {
      argv: vec!["sh".into(), "-c".into(), "echo hello world".into()],
      cwd: "/opt/my dir".into(),
    },
    RecordedCommand {
      argv: vec!["touch".into(), "done".into()],
      cwd: "/opt/my dir".into(),
    },
  ];

  let snippet = format_run_snippet(&commands).unwrap();
  assert!(snippet.starts_with("- run: |\n"), "{snippet}");
  assert!(snippet.contains("    apt-get update\n"), "{snippet}");
  assert!(snippet.contains("    cd '/opt/my dir'\n"), "{snippet}");
  assert!(snippet.contains("    sh -c 'echo hello world'\n"), "{snippet}");
  assert!(snippet.contains("    touch done\n"), "{snippet}");

  // Must parse back as a valid `needy_config::image::Layer` list.
  let layers: Vec<needy_config::image::Layer> = serde_yaml::from_str(&snippet).unwrap();
  assert_eq!(layers.len(), 1);
  let needy_config::image::Layer::Run(script) = &layers[0] else {
    panic!("expected run layer, got {:?}", layers[0]);
  };
  assert!(script.contains("apt-get update"), "{script}");
  assert!(script.contains("cd '/opt/my dir'"), "{script}");
}

#[test]
fn cli_parses_image_build_edit_rollback_and_preserves_build_from() {
  // Definition build by file path.
  let h = Harness::try_parse_from(["needy", "image", "build", "./agent.yaml"]).unwrap();
  let Top::Image(args) = h.cmd;
  assert!(args.allows_selection());

  // Definition build by profile.
  let h = Harness::try_parse_from(["needy", "image", "build", "--profile", "dev"]).unwrap();
  let Top::Image(args) = h.cmd;
  assert!(args.allows_selection());

  // Existing `image build --from` still parses and stays local.
  let h = Harness::try_parse_from(["needy", "image", "build", "--from", "example.com/actor:1"]).unwrap();
  let Top::Image(args) = h.cmd;
  assert!(!args.allows_selection());

  // `image edit` with file or profile.
  let h = Harness::try_parse_from(["needy", "image", "edit", "./agent.yaml"]).unwrap();
  let Top::Image(args) = h.cmd;
  assert!(args.allows_selection());
  let h = Harness::try_parse_from(["needy", "image", "edit", "--profile", "dev"]).unwrap();
  let Top::Image(args) = h.cmd;
  assert!(args.allows_selection());

  // `image rollback` arg parsing.
  assert_eq!(super::super::rollback::parse_args(None, &["./agent.yaml".into()]).unwrap(), (Some(PathBuf::from("./agent.yaml")), None, 1));
  assert_eq!(
    super::super::rollback::parse_args(None, &["./agent.yaml".into(), "@3".into()]).unwrap(),
    (Some(PathBuf::from("./agent.yaml")), None, 3)
  );
  assert_eq!(super::super::rollback::parse_args(Some(PathBuf::from("dev")), &["@2".into()]).unwrap(), (None, Some(PathBuf::from("dev")), 2));
  assert_eq!(super::super::rollback::parse_args(Some(PathBuf::from("dev")), &[]).unwrap(), (None, Some(PathBuf::from("dev")), 1));
  assert!(super::super::rollback::parse_args(None, &["./agent.yaml".into(), "@0".into()]).is_err());
  assert!(super::super::rollback::parse_args(Some(PathBuf::from("dev")), &["./agent.yaml".into()]).is_err());
  assert!(super::super::rollback::parse_args(None, &[]).is_err());
}
