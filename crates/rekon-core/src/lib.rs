//! Core of rekon: scanning a repository, storing one-line descriptions in `.rekon/`,
//! and asking a model backend for new ones. Has no UI dependencies.

pub mod apply;
pub mod backend;
pub mod check;
pub mod config;
pub mod context;
pub mod ctx;
pub mod hash;
pub mod init;
pub mod jobs;
pub mod model;
pub mod prompts;
pub mod scan;
pub mod segment;
pub mod setup;
pub mod store;
pub mod text;
pub mod view;

pub use ctx::Ctx;

/// Name of the map directory inside the repository root.
pub const MAP_DIR: &str = ".rekon";
