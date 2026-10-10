//! Core of rekon: scanning a repository, storing one-line descriptions in `.rekon/`,
//! and asking a model backend for new ones. Has no UI dependencies.

pub mod ai;
pub mod apply;
pub mod backend;
pub mod cards;
pub mod check;
pub mod collection;
pub mod config;
pub mod context;
pub mod contracts;
pub mod criteria;
pub mod ctx;
pub mod decisions;
pub mod element;
pub mod hash;
pub mod init;
pub mod jobs;
pub mod model;
pub mod outline;
pub mod prompts;
pub mod questions;
pub mod relations;
pub mod review;
pub mod scan;
pub mod segment;
pub mod setup;
pub mod store;
pub mod text;
pub mod trace;
pub mod view;

pub use ctx::Ctx;

/// Name of the map directory inside the repository root.
pub const MAP_DIR: &str = ".rekon";
