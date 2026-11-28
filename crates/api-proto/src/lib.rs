//! Generated types for the Needy daemon API.
//!
//! The schemas live in the repository's `proto/` directory. Run `make proto`
//! after editing a schema; the output under `src/gen` is committed.

#![allow(clippy::all, clippy::pedantic)]

pub mod diagnostic;

pub mod needy {
  pub mod v1 {
    include!("gen/needy/v1/needy.v1.rs");
  }
}

pub use needy::v1;

/// Boxide v1, the sandbox data-plane protocol (`proto/boxide/v1`). It names
/// nothing of needy so it can move to its own repository; see
/// `docs/ai/references/architecture/2026-11-27-boxide-protocol-spec.md`.
pub mod boxide {
  pub mod v1 {
    include!("gen/boxide/v1/boxide.v1.rs");
  }
}

#[cfg(test)]
#[path = "boxide_test.rs"]
mod boxide_test;
