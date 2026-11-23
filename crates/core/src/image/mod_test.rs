use super::*;
use needy_config::image::{FeatureOption, Layer};

fn digest(byte: char) -> Digest {
  Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn definition(scripts: &[&str]) -> Definition {
  Definition {
    root: None,
    pull_policy: Default::default(),
    layers: scripts.iter().map(|script| Layer::Run(script.to_string())).collect(),
    env: BTreeMap::new(),
    hooks: Default::default(),
  }
}

fn no_features() -> BTreeMap<String, ResolvedFeature> {
  BTreeMap::new()
}

fn feature_layer(reference: &str, options: &[(&str, FeatureOption)]) -> Layer {
  Layer::Feature {
    reference: reference.into(),
    options: options.iter().map(|(k, v)| ((*k).into(), v.clone())).collect(),
  }
}

fn resolved(byte: char, json: &str) -> ResolvedFeature {
  ResolvedFeature {
    digest: digest(byte),
    metadata: FeatureMetadata::parse(json).unwrap(),
  }
}

fn keys(plan: &Plan) -> Vec<Digest> {
  plan.steps.iter().map(|step| step.key).collect()
}

#[test]
fn a_digest_is_sha256_and_64_lowercase_hex_digits() {
  let text = format!("sha256:{}", "ab".repeat(32));
  let parsed = Digest::parse(&text).unwrap();
  assert_eq!(parsed.to_string(), text);
  assert_eq!(text.parse::<Digest>().unwrap(), parsed);
  assert_eq!(parsed.as_bytes(), &[0xab; 32]);
  assert_eq!(Digest::from_bytes([0xab; 32]), parsed);
  assert_eq!(parsed.hex(), "ab".repeat(32));
  let json = serde_json::to_string(&parsed).unwrap();
  assert_eq!(json, format!("\"{text}\""));
  assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), parsed);
  for bad in [
    "ab".repeat(32),
    format!("sha512:{}", "ab".repeat(32)),
    format!("sha256:{}", "AB".repeat(32)),
    format!("sha256:{}", "ab".repeat(31)),
  ] {
    let err = Digest::parse(&bad).unwrap_err().to_string();
    assert!(err.contains(&bad), "{err}");
  }
}

#[test]
fn one_step_per_run_layer_in_order() {
  let plan = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let inputs: Vec<_> = plan.steps.iter().map(|step| &step.input).collect();
  assert_eq!(inputs, [&Input::Run("one".into()), &Input::Run("two".into())]);
  assert_eq!(plan.root, digest('a'));
  assert_eq!(plan.user, User::default());
}

#[test]
fn keys_are_stable_and_distinct_per_step() {
  let a = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let b = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(keys(&a), keys(&b));
  assert_ne!(a.steps[0].key, a.steps[1].key);
  assert_ne!(a.steps[0].key, a.base);
}

#[test]
fn a_changed_script_changes_its_key_and_every_key_above_only() {
  let a = plan(&definition(&["one", "two", "three"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let b = plan(&definition(&["one", "TWO", "three"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(a.steps[0].key, b.steps[0].key);
  assert_ne!(a.steps[1].key, b.steps[1].key);
  assert_ne!(a.steps[2].key, b.steps[2].key);
}

#[test]
fn appending_a_layer_keeps_the_keys_below_it() {
  let a = plan(&definition(&["one"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let b = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(a.steps[0].key, b.steps[0].key);
}

#[test]
fn the_same_script_at_another_position_has_another_key() {
  let plan = plan(&definition(&["same", "same"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_ne!(plan.steps[0].key, plan.steps[1].key);
}

#[test]
fn another_root_changes_every_key() {
  let a = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let b = plan(&definition(&["one", "two"]), &digest('b'), &User::default(), None, &no_features()).unwrap();
  assert_ne!(a.base, b.base);
  assert!(keys(&a).iter().zip(keys(&b)).all(|(a, b)| *a != b));
}

// The profile's user is part of every cache key (decision A3 of the
// environments and snapshots design), so a build for one UID is never
// reused for another.
#[test]
fn another_user_name_uid_or_gid_changes_every_key() {
  let base = User::default();
  let a = plan(&definition(&["one", "two"]), &digest('a'), &base, None, &no_features()).unwrap();
  for user in [
    User {
      name: "other".into(),
      ..base.clone()
    },
    User {
      uid: 1001,
      ..base.clone()
    },
    User {
      gid: 1001,
      ..base.clone()
    },
  ] {
    let b = plan(&definition(&["one", "two"]), &digest('a'), &user, None, &no_features()).unwrap();
    assert_ne!(a.base, b.base, "{user:?}");
    assert!(keys(&a).iter().zip(keys(&b)).all(|(a, b)| *a != b), "{user:?}");
  }
}

#[test]
fn a_definition_without_layers_is_its_base() {
  let plan = plan(&definition(&[]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert!(plan.steps.is_empty());
  assert_eq!(plan.top(), &plan.base);
}

#[test]
fn the_top_is_the_last_steps_key() {
  let plan = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(plan.top(), &plan.steps[1].key);
}

#[test]
fn env_does_not_change_the_keys() {
  let a = plan(&definition(&["one"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  let mut with_env = definition(&["one"]);
  with_env.env.insert("EDITOR".into(), "vi".into());
  let b = plan(&with_env, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(keys(&a), keys(&b));
}

#[test]
fn without_a_path_the_image_path_is_the_system_path() {
  let mut image = definition(&[]);
  image.env.insert("EDITOR".into(), "vi".into());
  let plan = plan(&image, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(plan.path, SYSTEM_PATH);
  assert_eq!(plan.env, BTreeMap::from([("EDITOR".to_string(), "vi".to_string())]));
}

#[test]
fn the_image_path_expands_path_against_the_system_path_and_leaves_env() {
  let mut image = definition(&[]);
  image.env.insert("PATH".into(), "/opt/tool/bin:${PATH}:$PATH".into());
  let plan = plan(&image, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(plan.path, format!("/opt/tool/bin:{SYSTEM_PATH}:{SYSTEM_PATH}"));
  assert!(!plan.env.contains_key("PATH"));
}

#[test]
fn env_values_expand_path_and_keep_other_variables_as_written() {
  let mut image = definition(&[]);
  image.env.insert("TOOLS".into(), "${PATH}".into());
  image.env.insert("HOME_BIN".into(), "${HOME}/bin".into());
  let plan = plan(&image, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(plan.env["TOOLS"], SYSTEM_PATH);
  assert_eq!(plan.env["HOME_BIN"], "${HOME}/bin");
}

#[test]
fn without_a_commit_there_is_no_commit_layer() {
  let plan = plan(&definition(&["one"]), &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(plan.commit, None);
  assert!(!plan.commit_rebased());
}

#[test]
fn a_commit_made_on_the_top_is_not_rebased() {
  let top = *plan(&definition(&["one"]), &digest('a'), &User::default(), None, &no_features()).unwrap().top();
  let commit = Commit {
    layer: digest('c'),
    base: top,
  };
  let plan = plan(&definition(&["one"]), &digest('a'), &User::default(), Some(&commit), &no_features()).unwrap();
  assert_eq!(plan.commit, Some(commit));
  assert!(!plan.commit_rebased());
}

// A commit layer whose base changed is applied on the new base and reported
// as such (owner decision, 2026-11-17, in the images and run data spec).
#[test]
fn a_commit_made_on_another_base_stays_on_top_and_is_rebased() {
  let old = *plan(&definition(&["one"]), &digest('a'), &User::default(), None, &no_features()).unwrap().top();
  let commit = Commit {
    layer: digest('c'),
    base: old,
  };
  let user = User {
    uid: 1001,
    ..User::default()
  };
  for plan in [
    plan(&definition(&["one", "two"]), &digest('a'), &User::default(), Some(&commit), &no_features()).unwrap(),
    plan(&definition(&["one"]), &digest('b'), &User::default(), Some(&commit), &no_features()).unwrap(),
    plan(&definition(&["one"]), &digest('a'), &user, Some(&commit), &no_features()).unwrap(),
  ] {
    assert_eq!(plan.commit.as_ref(), Some(&commit));
    assert!(plan.commit_rebased());
  }
}

#[test]
fn feature_steps_include_digest_and_sanitized_options_in_cache_key() {
  let node_meta = r#"{
    "id": "node",
    "options": {
      "version": { "type": "string", "default": "lts" },
      "install-yarn": { "type": "boolean", "default": true }
    }
  }"#;
  let def_default = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/devcontainers/features/node:1", &[])],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let def_v22 = Definition {
    root: None,
    layers: vec![feature_layer(
      "ghcr.io/devcontainers/features/node:1",
      &[("version", FeatureOption::from("22"))],
    )],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let def_tag_v1_2 = Definition {
    root: None,
    layers: vec![feature_layer(
      "ghcr.io/devcontainers/features/node:1.2",
      &[],
    )],
    env: BTreeMap::new(),
    ..Default::default()
  };

  let features_a = BTreeMap::from([
    ("ghcr.io/devcontainers/features/node:1".into(), resolved('1', node_meta)),
    ("ghcr.io/devcontainers/features/node:1.2".into(), resolved('1', node_meta)),
  ]);
  let features_b = BTreeMap::from([("ghcr.io/devcontainers/features/node:1".into(), resolved('2', node_meta))]);

  let p_default = plan(&def_default, &digest('a'), &User::default(), None, &features_a).unwrap();
  let p_default_again = plan(&def_default, &digest('a'), &User::default(), None, &features_a).unwrap();
  let p_v22 = plan(&def_v22, &digest('a'), &User::default(), None, &features_a).unwrap();
  let p_diff_digest = plan(&def_default, &digest('a'), &User::default(), None, &features_b).unwrap();
  let p_same_digest_other_tag = plan(&def_tag_v1_2, &digest('a'), &User::default(), None, &features_a).unwrap();

  assert_eq!(
    p_default.steps[0].input,
    Input::Feature {
      reference: "ghcr.io/devcontainers/features/node:1".into(),
      digest: digest('1'),
      env: BTreeMap::from([
        ("INSTALL_YARN".into(), "true".into()),
        ("VERSION".into(), "lts".into()),
      ]),
    }
  );
  // Stable key for identical inputs.
  assert_eq!(p_default.steps[0].key, p_default_again.steps[0].key);
  // Same resolved digest and options -> same cache key regardless of tag.
  assert_eq!(p_default.steps[0].key, p_same_digest_other_tag.steps[0].key);
  // Different option -> different cache key.
  assert_ne!(p_default.steps[0].key, p_v22.steps[0].key);
  // Different digest -> different cache key.
  assert_ne!(p_default.steps[0].key, p_diff_digest.steps[0].key);
}

#[test]
fn depends_on_inserts_dependencies_before_dependents_recursively_without_duplicates() {
  let features = BTreeMap::from([
    (
      "ghcr.io/example/features/app:1".into(),
      resolved('3', r#"{ "id": "app", "dependsOn": { "ghcr.io/example/features/mid:1": { "mode": "fast" } } }"#),
    ),
    ("ghcr.io/example/features/mid:1".into(), resolved('2', r#"{ "id": "mid", "dependsOn": { "ghcr.io/example/features/base:1": {} } }"#)),
    ("ghcr.io/example/features/base:1".into(), resolved('1', r#"{ "id": "base" }"#)),
  ]);

  let def = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/example/features/app:1", &[]),
      Layer::Run("echo done".into()),
    ],
    env: BTreeMap::new(),
    ..Default::default()
  };

  let p = plan(&def, &digest('a'), &User::default(), None, &features).unwrap();
  let refs: Vec<&str> = p
    .steps
    .iter()
    .map(|s| match &s.input {
      Input::Feature { reference, .. } => reference.as_str(),
      Input::Run(script) => script.as_str(),
    })
    .collect();
  assert_eq!(
    refs,
    [
      "ghcr.io/example/features/base:1",
      "ghcr.io/example/features/mid:1",
      "ghcr.io/example/features/app:1",
      "echo done",
    ]
  );
  // Check dependency options from `dependsOn` were applied to `mid`.
  let Input::Feature { env, .. } = &p.steps[1].input else {
    panic!("expected feature step");
  };
  assert_eq!(env.get("MODE").map(String::as_str), Some("fast"));

  // When `base` is already listed before `app`, it is not duplicated.
  let def_explicit = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/example/features/base:1", &[]),
      feature_layer("ghcr.io/example/features/app:1", &[]),
    ],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let p2 = plan(&def_explicit, &digest('a'), &User::default(), None, &features).unwrap();
  assert_eq!(p2.steps.len(), 3);
}

#[test]
fn depends_on_cycle_returns_error_naming_cycle() {
  let features = BTreeMap::from([
    ("ghcr.io/example/features/a:1".into(), resolved('1', r#"{ "id": "a", "dependsOn": { "ghcr.io/example/features/b:1": {} } }"#)),
    ("ghcr.io/example/features/b:1".into(), resolved('2', r#"{ "id": "b", "dependsOn": { "ghcr.io/example/features/a:1": {} } }"#)),
  ]);
  let def = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/example/features/a:1", &[])],
    env: BTreeMap::new(),
    ..Default::default()
  };

  let err = plan(&def, &digest('a'), &User::default(), None, &features).unwrap_err().to_string();
  assert!(err.contains("cycle"), "{err}");
  assert!(err.contains("ghcr.io/example/features/a"), "{err}");
  assert!(err.contains("ghcr.io/example/features/b"), "{err}");
}

#[test]
fn installs_after_reorders_present_features_topologically_and_rejects_cycles() {
  let features = BTreeMap::from([
    ("ghcr.io/devcontainers/features/common-utils:2".into(), resolved('1', r#"{ "id": "common-utils" }"#)),
    (
      "ghcr.io/devcontainers/features/node:1".into(),
      resolved('2', r#"{ "id": "node", "installsAfter": ["ghcr.io/devcontainers/features/common-utils"] }"#),
    ),
  ]);

  // Valid order: common-utils before node.
  let valid = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/devcontainers/features/common-utils:2", &[]),
      feature_layer("ghcr.io/devcontainers/features/node:1", &[]),
    ],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let valid_plan = plan(&valid, &digest('a'), &User::default(), None, &features).unwrap();

  // Soft constraint: node alone (common-utils absent) is valid.
  let alone = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/devcontainers/features/node:1", &[])],
    env: BTreeMap::new(),
    ..Default::default()
  };
  plan(&alone, &digest('a'), &User::default(), None, &features).unwrap();

  // Reversed declaration order: node listed before common-utils is topologically
  // reordered per the Dev Container Features spec so common-utils installs first.
  let reversed = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/devcontainers/features/node:1", &[]),
      feature_layer("ghcr.io/devcontainers/features/common-utils:2", &[]),
    ],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let reordered = plan(&reversed, &digest('a'), &User::default(), None, &features).unwrap();
  let refs: Vec<&str> = reordered
    .steps
    .iter()
    .map(|s| match &s.input {
      Input::Feature { reference, .. } => reference.as_str(),
      Input::Run(script) => script.as_str(),
    })
    .collect();
  assert_eq!(
    refs,
    [
      "ghcr.io/devcontainers/features/common-utils:2",
      "ghcr.io/devcontainers/features/node:1",
    ]
  );
  assert_eq!(keys(&reordered), keys(&valid_plan));

  // Mutual installsAfter cycle is rejected cleanly.
  let cyclic = BTreeMap::from([
    ("ghcr.io/example/features/a:1".into(), resolved('1', r#"{ "id": "a", "installsAfter": ["ghcr.io/example/features/b"] }"#)),
    ("ghcr.io/example/features/b:1".into(), resolved('2', r#"{ "id": "b", "installsAfter": ["ghcr.io/example/features/a"] }"#)),
  ]);
  let cyclic_def = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/example/features/a:1", &[]),
      feature_layer("ghcr.io/example/features/b:1", &[]),
    ],
    env: BTreeMap::new(),
    ..Default::default()
  };
  let err = plan(&cyclic_def, &digest('a'), &User::default(), None, &cyclic).unwrap_err().to_string();
  assert!(err.contains("ghcr.io/example/features/a:1"), "{err}");
  assert!(err.contains("ghcr.io/example/features/b:1"), "{err}");
}

#[test]
fn container_env_expands_in_layer_order_and_merges_with_image_env() {
  let features = BTreeMap::from([
    (
      "ghcr.io/devcontainers/features/node:1".into(),
      resolved(
        '1',
        r#"{
          "id": "node",
          "containerEnv": {
            "NVM_DIR": "/usr/local/share/nvm",
            "PATH": "${NVM_DIR}/current/bin:${PATH}"
          }
        }"#,
      ),
    ),
    (
      "ghcr.io/devcontainers/features/python:1".into(),
      resolved(
        '2',
        r#"{
          "id": "python",
          "containerEnv": {
            "PIPX_BIN_DIR": "/usr/local/py-utils/bin",
            "PATH": "/usr/local/python/current/bin:$PIPX_BIN_DIR:${PATH}"
          }
        }"#,
      ),
    ),
  ]);

  let def = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/devcontainers/features/node:1", &[]),
      feature_layer("ghcr.io/devcontainers/features/python:1", &[]),
    ],
    env: BTreeMap::from([
      ("PATH".into(), "/custom/bin:${PATH}".into()),
      ("NODE_REF".into(), "${NVM_DIR}/versions".into()),
      ("KEEP_UNKNOWN".into(), "${HOME}/bin".into()),
    ]),
    ..Default::default()
  };

  let p = plan(&def, &digest('a'), &User::default(), None, &features).unwrap();
  assert_eq!(
    p.path,
    format!("/custom/bin:/usr/local/python/current/bin:/usr/local/py-utils/bin:/usr/local/share/nvm/current/bin:{SYSTEM_PATH}")
  );
  assert!(!p.env.contains_key("PATH"));
  assert_eq!(p.env["NVM_DIR"], "/usr/local/share/nvm");
  assert_eq!(p.env["PIPX_BIN_DIR"], "/usr/local/py-utils/bin");
  assert_eq!(p.env["NODE_REF"], "/usr/local/share/nvm/versions");
  assert_eq!(p.env["KEEP_UNKNOWN"], "${HOME}/bin");
}

#[test]
fn plan_refuses_missing_or_disallowed_feature() {
  let def = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/example/features/dind:1", &[])],
    env: BTreeMap::new(),
    ..Default::default()
  };

  let err = plan(&def, &digest('a'), &User::default(), None, &no_features()).unwrap_err().to_string();
  assert!(err.contains("ghcr.io/example/features/dind:1"), "{err}");

  let features = BTreeMap::from([("ghcr.io/example/features/dind:1".into(), resolved('1', r#"{ "id": "dind", "privileged": true }"#))]);
  let err = plan(&def, &digest('a'), &User::default(), None, &features).unwrap_err().to_string();
  assert!(err.contains("ghcr.io/example/features/dind:1"), "{err}");
  assert!(err.contains("privileged"), "{err}");
}

#[test]
fn hooks_collect_features_in_resolved_layer_order_before_image_hooks_without_affecting_keys() {
  let features = BTreeMap::from([
    ("ghcr.io/example/features/dep:1".into(), resolved('1', r#"{ "id": "dep", "postStartCommand": "dep-start" }"#)),
    (
      "ghcr.io/example/features/app:1".into(),
      resolved(
        '2',
        r#"{
          "id": "app",
          "dependsOn": { "ghcr.io/example/features/dep:1": {} },
          "postCreateCommand": "app-create",
          "postStartCommand": ["app-start", "--fast"]
        }"#,
      ),
    ),
  ]);

  let def_no_hooks = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/example/features/app:1", &[])],
    env: BTreeMap::new(),
    hooks: needy_config::image::Hooks::default(),
    ..Default::default()
  };
  let mut def_with_hooks = def_no_hooks.clone();
  def_with_hooks.hooks = needy_config::image::Hooks {
    start: vec!["image-start".into()],
    stop: vec!["image-stop".into()],
  };

  let p_no_hooks = plan(&def_no_hooks, &digest('a'), &User::default(), None, &features).unwrap();
  let p_with_hooks = plan(&def_with_hooks, &digest('a'), &User::default(), None, &features).unwrap();

  assert_eq!(keys(&p_no_hooks), keys(&p_with_hooks));
  assert_eq!(
    p_with_hooks.hooks.start,
    [
      "dep-start",
      "app-create",
      "'app-start' '--fast'",
      "image-start",
    ]
  );
  assert_eq!(p_with_hooks.hooks.stop, ["image-stop"]);
}

#[test]
fn collects_feature_sync_files_in_resolved_layer_order_with_expanded_home() {
  use needy_config::profile::OnConflict;

  let features = BTreeMap::from([
    (
      "ghcr.io/example/features/base-auth:1".into(),
      resolved(
        '1',
        r#"{
          "id": "base-auth",
          "customizations": {
            "needy": {
              "files": {
                "auth": {
                  "path": "~/.config/auth",
                  "on-conflict": { "token.json": "run" }
                }
              }
            }
          }
        }"#,
      ),
    ),
    (
      "ghcr.io/example/features/opencode:1".into(),
      resolved(
        '2',
        r#"{
          "id": "opencode",
          "dependsOn": { "ghcr.io/example/features/base-auth:1": {} },
          "customizations": {
            "needy": {
              "files": {
                "opencode": {
                  "path": "~/.opencode",
                  "store": "opencode-state",
                  "filter": ["- cache/"],
                  "on-conflict": { "history.jsonl": "lines" }
                }
              }
            }
          }
        }"#,
      ),
    ),
  ]);

  let def = Definition {
    root: None,
    layers: vec![feature_layer("ghcr.io/example/features/opencode:1", &[])],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  let user = User {
    name: "dev".into(),
    uid: 1001,
    gid: 1001,
  };

  let p = plan(&def, &digest('a'), &user, None, &features).unwrap();
  assert_eq!(
    p.sync_files,
    vec![
      FeatureSyncFile {
        name: "auth".into(),
        store: "auth".into(),
        path: "/home/dev/.config/auth".into(),
        filter: vec![],
        on_conflict: vec![("token.json".into(), OnConflict::Run)],
        feature: "ghcr.io/example/features/base-auth:1".into(),
      },
      FeatureSyncFile {
        name: "opencode".into(),
        store: "opencode-state".into(),
        path: "/home/dev/.opencode".into(),
        filter: vec!["- cache/".into()],
        on_conflict: vec![("history.jsonl".into(), OnConflict::Lines)],
        feature: "ghcr.io/example/features/opencode:1".into(),
      },
    ]
  );
}

#[test]
fn rejects_two_features_claiming_same_actor_path_or_mapping_name() {
  // Same actor path under different mapping names (including trailing slash normalization).
  let same_path = BTreeMap::from([
    (
      "ghcr.io/example/features/one:1".into(),
      resolved('1', r#"{ "id": "one", "customizations": { "needy": { "files": { "a": { "path": "~/.agent/" } } } } }"#),
    ),
    (
      "ghcr.io/example/features/two:1".into(),
      resolved('2', r#"{ "id": "two", "customizations": { "needy": { "files": { "b": { "path": "~/.agent" } } } } }"#),
    ),
  ]);
  let def = Definition {
    root: None,
    layers: vec![
      feature_layer("ghcr.io/example/features/one:1", &[]),
      feature_layer("ghcr.io/example/features/two:1", &[]),
    ],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  let err = plan(&def, &digest('a'), &User::default(), None, &same_path).unwrap_err().to_string();
  assert!(err.contains("/home/agent/.agent"), "{err}");
  assert!(err.contains("ghcr.io/example/features/one:1"), "{err}");
  assert!(err.contains("ghcr.io/example/features/two:1"), "{err}");

  // Same mapping name at different actor paths.
  let same_name = BTreeMap::from([
    (
      "ghcr.io/example/features/one:1".into(),
      resolved('1', r#"{ "id": "one", "customizations": { "needy": { "files": { "state": { "path": "~/.one" } } } } }"#),
    ),
    (
      "ghcr.io/example/features/two:1".into(),
      resolved('2', r#"{ "id": "two", "customizations": { "needy": { "files": { "state": { "path": "~/.two" } } } } }"#),
    ),
  ]);
  let err = plan(&def, &digest('a'), &User::default(), None, &same_name).unwrap_err().to_string();
  assert!(err.contains("state"), "{err}");
  assert!(err.contains("ghcr.io/example/features/one:1"), "{err}");
  assert!(err.contains("ghcr.io/example/features/two:1"), "{err}");
}

#[test]
fn plan_uses_default_root_when_root_is_omitted() {
  let def = Definition {
    root: None,
    layers: vec![Layer::Run("echo hi".into())],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  let p = plan(&def, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(p.root_image, DEFAULT_ROOT);
  assert_eq!(p.steps.len(), 1);
}

#[test]
fn custom_root_prepends_builtin_sshfs_without_duplicating_when_already_listed() {
  let features = BTreeMap::from([(BUILTIN_SSHFS.to_string(), resolved('5', r#"{ "id": "sshfs", "version": "1.0.0" }"#))]);

  // Custom root without sshfs gets `builtin:sshfs` prepended as step 0.
  let custom = Definition {
    root: Some("debian:bookworm-slim".into()),
    layers: vec![Layer::Run("echo hi".into())],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  assert!(needs_sshfs(&custom));
  let p = plan(&custom, &digest('a'), &User::default(), None, &features).unwrap();
  assert_eq!(p.root_image, "debian:bookworm-slim");
  assert_eq!(p.steps.len(), 2);
  assert!(matches!(&p.steps[0].input, Input::Feature { reference, .. } if reference == BUILTIN_SSHFS));
  assert_eq!(p.steps[1].input, Input::Run("echo hi".into()));

  // Custom root that already lists `builtin:sshfs` does not duplicate it.
  let explicit = Definition {
    root: Some("debian:bookworm-slim".into()),
    layers: vec![
      feature_layer(BUILTIN_SSHFS, &[]),
      Layer::Run("echo hi".into()),
    ],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  assert!(!needs_sshfs(&explicit));
  let p_explicit = plan(&explicit, &digest('a'), &User::default(), None, &features).unwrap();
  assert_eq!(p_explicit.steps.len(), 2);
  assert_eq!(p_explicit.steps[0].key, p.steps[0].key);

  // Needy's own base image does not prepend `builtin:sshfs`.
  let base = Definition {
    root: Some(needy_config::profile::BUILT_CONTAINER_IMAGE.into()),
    layers: vec![Layer::Run("echo hi".into())],
    env: BTreeMap::new(),
    hooks: Default::default(),
    ..Default::default()
  };
  assert!(!needs_sshfs(&base));
  let p_base = plan(&base, &digest('a'), &User::default(), None, &no_features()).unwrap();
  assert_eq!(p_base.steps.len(), 1);
}
