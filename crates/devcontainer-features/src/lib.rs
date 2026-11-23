//! Standalone Dev Container Features library.
//!
//! Provides:
//! - [`metadata`]: JSONC `devcontainer-feature.json` parsing (`FeatureMetadata`),
//!   option schemas/values, option name sanitization, and lifecycle commands.
//! - [`graph`]: transitive `dependsOn` expansion, cycle detection, and
//!   spec-compliant `installsAfter` topological sorting.
//! - [`fetch`]: OCI Distribution v2 feature fetching (`oci-client` +
//!   `docker_credential`), single-flight deduplication, and atomic
//!   content-addressed caching (`FeatureStore`).
//! - [`install`]: `install.sh` environment variables (`_REMOTE_USER*`,
//!   `_CONTAINER_USER*`) and POSIX wrapper script generation.
//! - [`digest`]: SHA-256 content digests (`sha256:<64 lowercase hex>`).

pub mod digest;
pub mod fetch;
pub mod graph;
pub mod install;
pub mod metadata;

pub use crate::digest::{Digest, InvalidDigest};
pub use crate::fetch::{FeatureStore, FetchError, FetchedFeature, PullPolicy, RefTarget, Reference, canonical_ref};
pub use crate::graph::{FeatureRequest, GraphError, OrderedFeature, resolve_order, resolve_order_excluding};
pub use crate::install::{InstallUser, feature_install_env, feature_install_script};
pub use crate::metadata::{
  FeatureMetadata, LifecycleCommand, LifecycleEntry, MetadataError, OptionSpec, OptionValue, sanitize_option_name, shell_join, untagged,
};

/// Top-level error type aggregating metadata, graph, and fetch errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Metadata(#[from] MetadataError),
  #[error(transparent)]
  Graph(#[from] GraphError),
  #[error(transparent)]
  Fetch(#[from] FetchError),
}
