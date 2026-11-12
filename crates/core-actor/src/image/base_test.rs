use std::path::Path;

use super::*;

#[test]
fn an_image_of_this_needys_base_abi_is_accepted() {
  check("needy-actor:latest", Some("1\n"), &rebuild_with("needy image build --linux-container")).unwrap();
}

#[test]
fn an_image_of_another_base_abi_is_refused_with_what_to_do() {
  let error = check("needy-actor:latest", Some("2\n"), &rebuild_with("needy image build --linux-container")).unwrap_err();

  assert_eq!(
    error.to_string(),
    "the actor image needy-actor:latest has base ABI 2, and this needy speaks base ABI 1; rebuild it with `needy image build \
     --linux-container` from this needy's version, or use a needy that speaks base ABI 2"
  );
}

#[test]
fn an_image_that_records_no_base_abi_is_refused_as_built_before_injection() {
  let error = check("needy-actor:latest", None, &rebuild_with("needy image build --linux-container")).unwrap_err();

  assert_eq!(error.found, None);
  assert!(error.to_string().starts_with("the actor image needy-actor:latest records no base ABI"), "{error}");
  assert!(error.to_string().ends_with("rebuild it with `needy image build --linux-container` from this needy's version"), "{error}");
}

#[test]
fn the_images_record_the_base_abi_this_needy_speaks() {
  let dockerfile = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../images/Dockerfile")).unwrap();

  let recorded = dockerfile.lines().find_map(|line| line.strip_prefix("ARG BASE_ABI="));

  assert_eq!(recorded, Some(ABI.to_string().as_str()), "images/Dockerfile's BASE_ABI");
  assert!(dockerfile.contains(&format!("LABEL {LABEL}=$BASE_ABI")), "the container image's label");
  assert!(dockerfile.contains(&format!("/out/{FILE} /{FILE}")), "the file the VM image targets export for the manifest");
}

#[test]
fn a_custom_image_of_another_base_abi_is_told_which_base_to_rebuild_from() {
  let advice = rebuild_from("needy image build --linux-container", None);

  let error = check("example.com/agent:1", None, &advice).unwrap_err();

  assert_eq!(
    error.to_string(),
    format!(
      "the actor image example.com/agent:1 records no base ABI: it is not built FROM a needy base image, or was built \
       before needy injected its guest at start; rebuild it FROM a base image of base ABI 1: `needy-actor:latest` after `needy image build --linux-container`, \
       or `ghcr.io/needy-dev/needy-actor:v{}`",
      env!("CARGO_PKG_VERSION")
    )
  );
}
