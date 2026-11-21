use super::*;

/// A directory of image files for `extends` tests.
fn files(entries: &[(&str, &str)]) -> tempfile::TempDir {
  let dir = tempfile::tempdir().unwrap();
  for (name, text) in entries {
    let path = dir.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
  }
  dir
}

fn load_text(yaml: &str) -> Result<Definition, crate::Error> {
  load(&Source::text(yaml, "/base"))
}

fn run(script: &str) -> Layer {
  Layer::Run(script.into())
}

#[test]
fn an_image_file_has_a_root_run_layers_and_env() {
  let image = load_text(
    r#"
root: example.com/root:1
layers:
  - run: apt-get install -y jq
  - run: |
      echo one
      echo two
env: { EDITOR: vi }
"#,
  )
  .unwrap();

  assert_eq!(image.root.as_deref(), Some("example.com/root:1"));
  assert_eq!(image.layers, [run("apt-get install -y jq"), run("echo one\necho two\n")]);
  assert_eq!(image.env["EDITOR"], "vi");
}

#[test]
fn an_empty_image_file_is_an_empty_definition() {
  assert_eq!(load_text("# nothing yet\n").unwrap(), Definition::default());
}

#[test]
fn layers_append_along_extends_parent_first() {
  let dir = files(&[
    ("base.image.yaml", "root: example.com/root:1\nlayers: [{ run: one }, { run: two }]\nenv: { A: a, B: b }\n"),
    ("images/child.image.yaml", "extends: ../base.image.yaml\nlayers: [{ run: three }]\nenv: { B: c }\n"),
  ]);

  let image = load(&Source::file(dir.path().join("images/child.image.yaml"))).unwrap();

  assert_eq!(image.root.as_deref(), Some("example.com/root:1"));
  assert_eq!(image.layers, [run("one"), run("two"), run("three")]);
  assert_eq!((image.env["A"].as_str(), image.env["B"].as_str()), ("a", "c"));
}

#[test]
fn a_child_can_drop_its_parents_layers_with_null() {
  let dir = files(&[
    ("base.image.yaml", "layers: [{ run: one }]\n"),
    ("child.image.yaml", "extends: base.image.yaml\nlayers: null\n"),
  ]);

  let image = load(&Source::file(dir.path().join("child.image.yaml"))).unwrap();

  assert!(image.layers.is_empty());
}

#[test]
fn a_feature_layer_parses_with_string_bool_and_integer_options() {
  let image = load_text(
    r#"
root: example.com/root:1
layers:
  - feature: ghcr.io/devcontainers/features/node:1
    with:
      version: "22"
      nodeGypDependencies: true
      nvmVersion: 2
  - feature: ./features/local
"#,
  )
  .unwrap();

  assert_eq!(
    image.layers,
    [
      Layer::Feature {
        reference: "ghcr.io/devcontainers/features/node:1".into(),
        options: BTreeMap::from([
          ("nodeGypDependencies".into(), FeatureOption::Bool(true)),
          ("nvmVersion".into(), FeatureOption::Int(2)),
          ("version".into(), FeatureOption::String("22".into())),
        ]),
      },
      Layer::Feature {
        reference: "./features/local".into(),
        options: BTreeMap::new(),
      },
    ]
  );
}

#[test]
fn a_child_feature_of_the_same_reference_replaces_the_parents_in_place() {
  let dir = files(&[
    (
      "base.image.yaml",
      "layers:\n  - feature: ghcr.io/devcontainers/features/node:1\n    with: { version: \"20\" }\n  - run: one\n  - feature: ghcr.io/devcontainers/features/git@sha256:aaaa\n",
    ),
    (
      "child.image.yaml",
      "extends: base.image.yaml\nlayers:\n  - feature: ghcr.io/devcontainers/features/node:2\n    with: { version: \"22\" }\n  - feature: ghcr.io/devcontainers/features/git:1\n  - feature: ghcr.io/devcontainers/features/python:1\n",
    ),
  ]);

  let image = load(&Source::file(dir.path().join("child.image.yaml"))).unwrap();

  assert_eq!(
    image.layers,
    [
      Layer::Feature {
        reference: "ghcr.io/devcontainers/features/node:2".into(),
        options: BTreeMap::from([("version".into(), FeatureOption::String("22".into()))]),
      },
      run("one"),
      Layer::Feature {
        reference: "ghcr.io/devcontainers/features/git:1".into(),
        options: BTreeMap::new(),
      },
      Layer::Feature {
        reference: "ghcr.io/devcontainers/features/python:1".into(),
        options: BTreeMap::new(),
      },
    ]
  );
}

#[test]
fn untagged_strips_tag_and_digest_while_keeping_registry_port() {
  assert_eq!(untagged("ghcr.io/devcontainers/features/node:1"), "ghcr.io/devcontainers/features/node");
  assert_eq!(untagged("ghcr.io/devcontainers/features/node:1.2.3"), "ghcr.io/devcontainers/features/node");
  assert_eq!(untagged("ghcr.io/devcontainers/features/node@sha256:abcd"), "ghcr.io/devcontainers/features/node");
  assert_eq!(untagged("ghcr.io/devcontainers/features/node:1@sha256:abcd"), "ghcr.io/devcontainers/features/node");
  assert_eq!(untagged("localhost:5000/features/node:1"), "localhost:5000/features/node");
  assert_eq!(untagged("localhost:5000/features/node"), "localhost:5000/features/node");
  assert_eq!(untagged("./features/x"), "./features/x");
}

#[test]
fn a_layer_is_one_kind() {
  let err = load_text("layers: [{ env: { A: a } }]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0].env: unknown field `env`"), "{err}");

  let err = load_text("layers: [{}]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0]: a layer needs `run` or `feature`"), "{err}");

  let err = load_text("layers: [{ run: one, feature: ./features/x }]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0]: a layer is one of `run` or `feature`"), "{err}");

  let err = load_text("layers: [{ run: one, with: { version: \"1\" } }]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0]: `with` sets a `feature` layer's options"), "{err}");

  let err = load_text("layers: [{ feature: \"\" }]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0]: `feature` reference cannot be empty"), "{err}");

  let err = load_text("layers: [{ feature: ./features/x, with: { version: 3.12 } }]\n").unwrap_err().to_string();
  assert!(err.contains("layers[0].with.version"), "{err}");
}

#[test]
fn hooks_parse_strings_and_argv_arrays() {
  let image = load_text(
    r#"
root: example.com/root:1
hooks:
  start:
    - ./prepare.sh
    - ["git", "config", "user.name", "Agent Smith"]
  stop:
    - ./flush.sh
    - ["sync", "it's done"]
"#,
  )
  .unwrap();

  assert_eq!(image.hooks.start, ["./prepare.sh", "'git' 'config' 'user.name' 'Agent Smith'",]);
  assert_eq!(image.hooks.stop, ["./flush.sh", "'sync' 'it'\\''s done'"]);
}

#[test]
fn hooks_append_along_extends_parent_first_and_null_clears_them() {
  let dir = files(&[
    ("base.image.yaml", "hooks:\n  start: [base-1, base-2]\n  stop: [stop-1]\n"),
    ("child.image.yaml", "extends: base.image.yaml\nhooks:\n  start: [child-1]\n  stop: [stop-2]\n"),
    ("cleared.image.yaml", "extends: base.image.yaml\nhooks:\n  start: null\n"),
  ]);

  let child = load(&Source::file(dir.path().join("child.image.yaml"))).unwrap();
  assert_eq!(child.hooks.start, ["base-1", "base-2", "child-1"]);
  assert_eq!(child.hooks.stop, ["stop-1", "stop-2"]);

  let cleared = load(&Source::file(dir.path().join("cleared.image.yaml"))).unwrap();
  assert!(cleared.hooks.start.is_empty());
  assert_eq!(cleared.hooks.stop, ["stop-1"]);
}

#[test]
fn empty_hook_argv_array_is_rejected() {
  let err = load_text("hooks: { start: [[]] }\n").unwrap_err().to_string();
  assert!(err.contains("empty"), "{err}");
}

#[test]
fn unknown_keys_are_errors() {
  let err = load_text("rooot: example.com/root:1\n").unwrap_err().to_string();
  assert!(err.contains("unknown field `rooot`"), "{err}");

  // `attach` hook is deferred; until then it is an unknown key.
  let err = load_text("hooks: { attach: [./welcome.sh] }\n").unwrap_err().to_string();
  assert!(err.contains("unknown field `attach`"), "{err}");
}

#[test]
fn an_image_file_extends_no_built_in() {
  let err = load_text("extends: builtin:agents\n").unwrap_err().to_string();

  assert_eq!(err, "image (text in /base): no built-in image named \"agents\"");
}

#[test]
fn untagged_preserves_builtin_feature_references() {
  assert_eq!(untagged("builtin:sshfs"), "builtin:sshfs");
  assert_eq!(untagged("builtin:codex"), "builtin:codex");
  assert_eq!(untagged("builtin:pi"), "builtin:pi");
  assert_eq!(untagged("builtin:codex@sha256:abc"), "builtin:codex");
}

#[test]
fn effective_root_defaults_to_pinned_default_root() {
  let without_root = load_text("layers: []\n").unwrap();
  assert_eq!(without_root.root, None);
  assert_eq!(without_root.effective_root(), DEFAULT_ROOT);

  let with_root = load_text("root: debian:bookworm-slim\n").unwrap();
  assert_eq!(with_root.effective_root(), "debian:bookworm-slim");
}
