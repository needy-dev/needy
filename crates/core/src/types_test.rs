use super::Origin;

#[test]
fn an_origins_listed_name_is_its_trace_name() {
  for origin in [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ] {
    assert_eq!(serde_json::to_value(origin).unwrap(), origin.as_str());
  }
}

#[test]
fn file_transfers_and_mounts_are_needys_own_processes() {
  let internal: Vec<Origin> = [
    Origin::Cli,
    Origin::Fs,
    Origin::Files,
    Origin::ActorProxy,
    Origin::ActorMount,
  ]
  .into_iter()
  .filter(|o| o.is_internal())
  .collect();
  assert_eq!(internal, [Origin::Fs, Origin::Files, Origin::ActorMount]);
}
