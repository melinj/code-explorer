//! cv-server (B7): the snapshot indexing pipeline and the HTTP routes behind the `cv` binary.

pub mod agent_api;
pub mod auth;
pub mod channels;
pub mod extras_api;
pub mod grades;
pub mod incremental;
pub mod index;
pub mod layout_api;
pub mod layout_input;
pub mod mcp;
pub mod parse_cache;
pub mod readside;
pub mod repo_api;
pub mod review_api;
pub mod routes;
pub mod scheduler;
pub mod ui;
pub mod view_store;
pub mod workspaces;
