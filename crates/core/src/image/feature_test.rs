use std::collections::BTreeMap;

use needy_config::image::FeatureOption;

use super::*;

#[test]
fn parses_official_devcontainer_feature_json_and_fills_defaults() {
  let json = r#"{
    "id": "node",
    "version": "2.1.2",
    "name": "Node.js (via nvm) and yarn",
    "description": "Installs Node.js, nvm, yarn, and pnpm",
    "options": {
      "version": {
        "type": "string",
        "default": "lts",
        "proposals": ["lts", "latest", "22"]
      },
      "nodeGypDependencies": {
        "type": "boolean",
        "default": true
      },
      "nvmInstallPath": {
        "type": "string",
        "default": "/usr/local/share/nvm"
      },
      "optionalNoDefault": {
        "type": "string"
      }
    },
    "containerEnv": {
      "NVM_DIR": "/usr/local/share/nvm",
      "NVM_SYMLINK_CURRENT": "true",
      "PATH": "/usr/local/share/nvm/current/bin:${PATH}"
    },
    "installsAfter": [
      "ghcr.io/devcontainers/features/common-utils"
    ],
    "dependsOn": {
      "ghcr.io/devcontainers/features/git:1": {
        "version": "os-provided"
      }
    }
  }"#;

  let meta = FeatureMetadata::parse(json).unwrap();
  assert_eq!(meta.id, "node");
  assert_eq!(meta.version.as_deref(), Some("2.1.2"));
  assert_eq!(meta.name.as_deref(), Some("Node.js (via nvm) and yarn"));
  assert_eq!(meta.installs_after, ["ghcr.io/devcontainers/features/common-utils"]);
  assert_eq!(
    meta.depends_on["ghcr.io/devcontainers/features/git:1"],
    BTreeMap::from([("version".into(), OptionValue::String("os-provided".into()))])
  );

  // Defaults are filled in; user overrides win; undeclared options pass through.
  let user_opts = BTreeMap::from([
    ("version".into(), FeatureOption::String("22".into())),
    ("nodeGypDependencies".into(), FeatureOption::Bool(false)),
    ("custom-opt".into(), FeatureOption::Int(42)),
  ]);
  let env = meta.resolve_options(&user_opts);
  assert_eq!(
    env,
    BTreeMap::from([
      ("CUSTOM_OPT".into(), "42".into()),
      ("NODEGYPDEPENDENCIES".into(), "false".into()),
      ("NVMINSTALLPATH".into(), "/usr/local/share/nvm".into()),
      ("VERSION".into(), "22".into()),
    ])
  );
}

#[test]
fn sanitizes_option_names_per_dev_container_features_spec() {
  assert_eq!(sanitize_option_name("version"), "VERSION");
  assert_eq!(sanitize_option_name("install-zsh"), "INSTALL_ZSH");
  assert_eq!(sanitize_option_name("nodeGypDependencies"), "NODEGYPDEPENDENCIES");
  assert_eq!(sanitize_option_name("foo.bar/baz"), "FOO_BAR_BAZ");
  assert_eq!(sanitize_option_name("123foo"), "_FOO");
  assert_eq!(sanitize_option_name("__123_foo-bar"), "_FOO_BAR");
  assert_eq!(sanitize_option_name("_opt"), "_OPT");
  assert_eq!(sanitize_option_name("0"), "_");
}

#[test]
fn refuses_privileged_cap_add_security_opt_mounts_and_entrypoint() {
  let cases = [
    (r#"{"id": "dind", "privileged": true}"#, "privileged"),
    (r#"{"id": "caps", "capAdd": ["SYS_PTRACE"]}"#, "capAdd"),
    (r#"{"id": "sec", "securityOpt": ["seccomp=unconfined"]}"#, "securityOpt"),
    (r#"{"id": "mnt", "mounts": ["source=dind-var-lib-docker,target=/var/lib/docker,type=volume"]}"#, "mounts"),
    (r#"{"id": "mnt-obj", "mounts": [{"source": "s", "target": "/t", "type": "volume"}]}"#, "mounts"),
    (r#"{"id": "sshd", "entrypoint": "/usr/local/share/ssh-init.sh"}"#, "entrypoint"),
  ];

  for (json, property) in cases {
    let meta = FeatureMetadata::parse(json).unwrap();
    let reference = format!("ghcr.io/example/features/{}:1", meta.id);
    let err = meta.check_allowed(&reference).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&reference), "error should name feature `{reference}`: {msg}");
    assert!(msg.contains(property), "error should name property `{property}`: {msg}");
  }
}

#[test]
fn allows_init_and_empty_refusal_fields() {
  let json = r#"{
    "id": "common-utils",
    "init": true,
    "privileged": false,
    "capAdd": [],
    "securityOpt": [],
    "mounts": [],
    "entrypoint": ""
  }"#;
  let meta = FeatureMetadata::parse(json).unwrap();
  assert!(meta.init);
  meta.check_allowed("ghcr.io/devcontainers/features/common-utils:2").unwrap();
}

#[test]
fn parses_lifecycle_hooks_in_string_array_and_object_forms() {
  let json = r#"{
    "id": "demo",
    "onCreateCommand": "echo build-only",
    "updateContentCommand": "echo update",
    "postCreateCommand": ["echo", "hello world", "it's ok"],
    "postStartCommand": {
      "b-second": ["service", "start"],
      "a-first": "echo first"
    },
    "postAttachCommand": "echo attach"
  }"#;

  let meta = FeatureMetadata::parse(json).unwrap();
  assert_eq!(
    meta.start_hooks(),
    [
      "echo update",
      "'echo' 'hello world' 'it'\\''s ok'",
      "echo first",
      "'service' 'start'",
    ]
  );
}

#[test]
fn empty_lifecycle_hooks_produce_no_start_hooks() {
  let json = r#"{
    "id": "empty-hooks",
    "postCreateCommand": [],
    "postStartCommand": {}
  }"#;

  let meta = FeatureMetadata::parse(json).unwrap();
  assert!(meta.start_hooks().is_empty());
}

#[test]
fn parses_customizations_needy_files_with_store_filter_and_on_conflict() {
  use needy_config::profile::OnConflict;

  let json = r#"{
    "id": "opencode",
    "version": "1.0.0",
    "customizations": {
      "vscode": { "extensions": ["example.ext"] },
      "needy": {
        "files": {
          "opencode": {
            "path": "~/.opencode",
            "filter": ["- cache/", "- *.log"],
            "on-conflict": {
              "auth.json": "run",
              "history.jsonl": "lines",
              "installation_id": "store"
            }
          },
          "opencode-config": {
            "path": "~/.config/opencode",
            "store": "opencode/config"
          }
        }
      }
    }
  }"#;

  let meta = FeatureMetadata::parse(json).unwrap();
  meta.check_allowed("ghcr.io/example/features/opencode:1").unwrap();

  let needy = meta.customizations.needy.as_ref().expect("customizations.needy");
  assert_eq!(needy.files.len(), 2);

  let main = &needy.files["opencode"];
  assert_eq!(main.path, "~/.opencode");
  assert_eq!(main.store, None);
  assert_eq!(main.filter, ["- cache/", "- *.log"]);
  assert_eq!(
    main.on_conflict,
    vec![
      ("auth.json".to_string(), OnConflict::Run),
      ("history.jsonl".to_string(), OnConflict::Lines),
      ("installation_id".to_string(), OnConflict::Store),
    ]
  );

  let cfg = &needy.files["opencode-config"];
  assert_eq!(cfg.path, "~/.config/opencode");
  assert_eq!(cfg.store.as_deref(), Some("opencode/config"));
  assert!(cfg.filter.is_empty());
  assert!(cfg.on_conflict.is_empty());
}

#[test]
fn accepts_merge_driver_in_customizations_needy_files_on_conflict() {
  use needy_config::profile::OnConflict;

  let json = r#"{
    "id": "opencode",
    "customizations": {
      "needy": {
        "files": {
          "opencode": {
            "path": "~/.opencode",
            "on-conflict": {
              "opencode.db*": { "driver": "needy-merge-sqlite" }
            }
          }
        }
      }
    }
  }"#;

  let meta = FeatureMetadata::parse(json).unwrap();
  meta.check_allowed("ghcr.io/example/features/opencode:1").unwrap();
  let needy = meta.customizations.needy.as_ref().expect("customizations.needy");
  assert_eq!(needy.files["opencode"].on_conflict, vec![("opencode.db*".to_string(), OnConflict::Driver("needy-merge-sqlite".into()))]);
}

#[test]
fn rejects_invalid_sync_file_store_names_and_paths() {
  let cases = [
    (r#"{"id":"a","customizations":{"needy":{"files":{"../escape":{"path":"~/.a"}}}}}"#, "../escape"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a/b":{"path":"~/.a"}}}}}"#, "a/b"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a":{"path":"~/.a","store":"../escape"}}}}}"#, "../escape"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a":{"path":"~/.a","store":"/etc/passwd"}}}}}"#, "/etc/passwd"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a":{"path":"~/.a","store":"~/home"}}}}}"#, "~/home"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a":{"path":""}}}}}"#, "path"),
    (r#"{"id":"a","customizations":{"needy":{"files":{"a":{"path":"relative/dir"}}}}}"#, "relative/dir"),
  ];

  for (json, expected_fragment) in cases {
    let meta = FeatureMetadata::parse(json).unwrap();
    let err = meta.check_allowed("ghcr.io/example/features/a:1").unwrap_err().to_string();
    assert!(err.contains("ghcr.io/example/features/a:1"), "error should name feature: {err}");
    assert!(err.contains(expected_fragment), "error should mention {expected_fragment:?}: {err}");
  }
}
