//! Per-app profiles: the stored rules, and the pure function that picks one.
//!
//! The *capture* half lives in `kea-platform` (`textio::appctx`), because it
//! is AX and NSWorkspace. This half is deliberately OS-free: it takes a
//! [`ProfileQuery`] — two borrowed strings — rather than a platform
//! `AppContext`, which is what keeps `kea-core` from depending on
//! `kea-platform` (and `kea-platform` from dragging in `sqlx` and `keyring`).
//! `kea-features` depends on both and is the natural place to build the query
//! from a captured context.

pub mod profile;
pub mod resolve;

pub use profile::{AppProfile, InsertionMode, ProfileQuery, Specificity};
pub use resolve::resolve_profile;
