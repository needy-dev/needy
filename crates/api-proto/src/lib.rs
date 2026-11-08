//! Generated types for the Needy daemon API.
//!
//! The schemas live in the repository's `proto/` directory. Run `make proto`
//! after editing a schema; the output under `src/gen` is committed.

#![allow(clippy::all, clippy::pedantic)]

pub mod needy {
    pub mod v1 {
        include!("gen/needy/v1/needy.v1.rs");
    }
}

pub use needy::v1;
