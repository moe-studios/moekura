//! Domain types and pure logic for uwuubooru.
//!
//! Nothing in this crate performs IO; it is shared by the database, web and
//! binary crates.

pub mod accounts;
pub mod config;
pub mod jobs;
pub mod permissions;
pub mod posts;
pub mod settings;
pub mod tags;
pub mod tokens;
