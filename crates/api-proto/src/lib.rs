//! Generated types for the Needy daemon API.
//!
//! The schemas live in the repository's `proto/` directory. Run
//! `buf generate` from there after editing a schema; the output under
//! `src/gen` is committed.

#![allow(clippy::all, clippy::pedantic)]

pub mod needy {
    pub mod api {
        pub mod daemon {
            pub mod v1 {
                include!("gen/needy/api/daemon/v1/needy.api.daemon.v1.rs");
            }
        }

        pub mod runtime {
            pub mod v1 {
                include!("gen/needy/api/runtime/v1/needy.api.runtime.v1.rs");
            }
        }
    }
}

pub use needy::api::{daemon, runtime};
