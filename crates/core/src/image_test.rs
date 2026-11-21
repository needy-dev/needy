use super::*;
use needy_config::image::Layer;

fn digest(byte: char) -> Digest {
  Digest::parse(&format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn definition(scripts: &[&str]) -> Definition {
  Definition {
    root: Some("example.com/root:1".into()),
    layers: scripts.iter().map(|script| Layer::Run(script.to_string())).collect(),
    env: BTreeMap::new(),
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
  let plan = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  let inputs: Vec<_> = plan.steps.iter().map(|step| &step.input).collect();
  assert_eq!(inputs, [&Input::Run("one".into()), &Input::Run("two".into())]);
  assert_eq!(plan.root, digest('a'));
  assert_eq!(plan.user, User::default());
}

#[test]
fn keys_are_stable_and_distinct_per_step() {
  let a = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  let b = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  assert_eq!(keys(&a), keys(&b));
  assert_ne!(a.steps[0].key, a.steps[1].key);
  assert_ne!(a.steps[0].key, a.base);
}

#[test]
fn a_changed_script_changes_its_key_and_every_key_above_only() {
  let a = plan(&definition(&["one", "two", "three"]), &digest('a'), &User::default(), None);
  let b = plan(&definition(&["one", "TWO", "three"]), &digest('a'), &User::default(), None);
  assert_eq!(a.steps[0].key, b.steps[0].key);
  assert_ne!(a.steps[1].key, b.steps[1].key);
  assert_ne!(a.steps[2].key, b.steps[2].key);
}

#[test]
fn appending_a_layer_keeps_the_keys_below_it() {
  let a = plan(&definition(&["one"]), &digest('a'), &User::default(), None);
  let b = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  assert_eq!(a.steps[0].key, b.steps[0].key);
}

#[test]
fn the_same_script_at_another_position_has_another_key() {
  let plan = plan(&definition(&["same", "same"]), &digest('a'), &User::default(), None);
  assert_ne!(plan.steps[0].key, plan.steps[1].key);
}

#[test]
fn another_root_changes_every_key() {
  let a = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  let b = plan(&definition(&["one", "two"]), &digest('b'), &User::default(), None);
  assert_ne!(a.base, b.base);
  assert!(keys(&a).iter().zip(keys(&b)).all(|(a, b)| *a != b));
}

// The profile's user is part of every cache key (decision A3 of the
// environments and snapshots design), so a build for one UID is never
// reused for another.
#[test]
fn another_user_name_uid_or_gid_changes_every_key() {
  let base = User::default();
  let a = plan(&definition(&["one", "two"]), &digest('a'), &base, None);
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
    let b = plan(&definition(&["one", "two"]), &digest('a'), &user, None);
    assert_ne!(a.base, b.base, "{user:?}");
    assert!(keys(&a).iter().zip(keys(&b)).all(|(a, b)| *a != b), "{user:?}");
  }
}

#[test]
fn a_definition_without_layers_is_its_base() {
  let plan = plan(&definition(&[]), &digest('a'), &User::default(), None);
  assert!(plan.steps.is_empty());
  assert_eq!(plan.top(), &plan.base);
}

#[test]
fn the_top_is_the_last_steps_key() {
  let plan = plan(&definition(&["one", "two"]), &digest('a'), &User::default(), None);
  assert_eq!(plan.top(), &plan.steps[1].key);
}

#[test]
fn env_does_not_change_the_keys() {
  let a = plan(&definition(&["one"]), &digest('a'), &User::default(), None);
  let mut with_env = definition(&["one"]);
  with_env.env.insert("EDITOR".into(), "vi".into());
  let b = plan(&with_env, &digest('a'), &User::default(), None);
  assert_eq!(keys(&a), keys(&b));
}

#[test]
fn without_a_path_the_image_path_is_the_system_path() {
  let mut image = definition(&[]);
  image.env.insert("EDITOR".into(), "vi".into());
  let plan = plan(&image, &digest('a'), &User::default(), None);
  assert_eq!(plan.path, SYSTEM_PATH);
  assert_eq!(plan.env, BTreeMap::from([("EDITOR".to_string(), "vi".to_string())]));
}

#[test]
fn the_image_path_expands_path_against_the_system_path_and_leaves_env() {
  let mut image = definition(&[]);
  image.env.insert("PATH".into(), "/opt/tool/bin:${PATH}:$PATH".into());
  let plan = plan(&image, &digest('a'), &User::default(), None);
  assert_eq!(plan.path, format!("/opt/tool/bin:{SYSTEM_PATH}:{SYSTEM_PATH}"));
  assert!(!plan.env.contains_key("PATH"));
}

#[test]
fn env_values_expand_path_and_keep_other_variables_as_written() {
  let mut image = definition(&[]);
  image.env.insert("TOOLS".into(), "${PATH}".into());
  image.env.insert("HOME_BIN".into(), "${HOME}/bin".into());
  let plan = plan(&image, &digest('a'), &User::default(), None);
  assert_eq!(plan.env["TOOLS"], SYSTEM_PATH);
  assert_eq!(plan.env["HOME_BIN"], "${HOME}/bin");
}

#[test]
fn without_a_commit_there_is_no_commit_layer() {
  let plan = plan(&definition(&["one"]), &digest('a'), &User::default(), None);
  assert_eq!(plan.commit, None);
  assert!(!plan.commit_rebased());
}

#[test]
fn a_commit_made_on_the_top_is_not_rebased() {
  let top = *plan(&definition(&["one"]), &digest('a'), &User::default(), None).top();
  let commit = Commit {
    layer: digest('c'),
    base: top,
  };
  let plan = plan(&definition(&["one"]), &digest('a'), &User::default(), Some(&commit));
  assert_eq!(plan.commit, Some(commit));
  assert!(!plan.commit_rebased());
}

// A commit layer whose base changed is applied on the new base and reported
// as such (owner decision, 2026-11-17, in the images and run data spec).
#[test]
fn a_commit_made_on_another_base_stays_on_top_and_is_rebased() {
  let old = *plan(&definition(&["one"]), &digest('a'), &User::default(), None).top();
  let commit = Commit {
    layer: digest('c'),
    base: old,
  };
  let user = User {
    uid: 1001,
    ..User::default()
  };
  for plan in [
    plan(&definition(&["one", "two"]), &digest('a'), &User::default(), Some(&commit)),
    plan(&definition(&["one"]), &digest('b'), &User::default(), Some(&commit)),
    plan(&definition(&["one"]), &digest('a'), &user, Some(&commit)),
  ] {
    assert_eq!(plan.commit.as_ref(), Some(&commit));
    assert!(plan.commit_rebased());
  }
}
