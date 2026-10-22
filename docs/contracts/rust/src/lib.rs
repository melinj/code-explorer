//! Boundaries between the Code Viewer backend crates (see docs/WORKSTREAMS.md).
//!
//! Rules every crate follows:
//! - Service traits are synchronous and run on the blocking pool or rayon. Only `cv-server` and
//!   `cv-mcp` are async (tokio); they call into services through `spawn_blocking`.
//! - The core never names a language or a protocol. Language-specific work lives behind
//!   [`lang::LanguagePack`], protocol-specific work behind [`channel::ChannelPack`].
//! - Types shared with the web client and MCP come from `cv-contracts` (the port of
//!   docs/contracts/*.ts); [`contracts`] stands in for them here.
//! - Lines are 1-based inclusive; columns are 0-based UTF-16 code units at the API boundary
//!   (language packs convert from tree-sitter byte columns).

pub mod channel;
pub mod contracts;
pub mod ids;
pub mod index;
pub mod lang;
pub mod review;
pub mod vcs;
