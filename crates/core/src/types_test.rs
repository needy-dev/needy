use super::{Origin, Principal};

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

#[test]
fn a_principal_reads_back_as_it_is_written() {
  for (principal, text) in [
    (Principal::Operator, "operator"),
    (Principal::Client("laptop".into()), "client:laptop"),
    (Principal::Runner("kvm-1".into()), "runner:kvm-1"),
    (Principal::Actor("0a1b2c3d".into()), "actor:0a1b2c3d"),
  ] {
    assert_eq!(principal.to_string(), text);
    assert_eq!(text.parse::<Principal>().unwrap(), principal);
    assert_eq!(serde_json::to_string(&principal).unwrap(), format!("\"{text}\""));
  }
}

#[test]
fn a_principal_of_an_unknown_kind_or_without_a_name_is_refused() {
  for text in ["", "client:", "user:me", "operators"] {
    assert!(text.parse::<Principal>().is_err(), "{text}");
  }
}
