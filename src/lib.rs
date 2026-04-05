// lib.rs — shared library for modules used by both server and admin CLI

pub mod config;
pub mod db;
pub mod error;
pub mod favicon;
pub mod media;
pub mod models;
pub mod pending_fs;
pub mod templates;
#[cfg(test)]
pub mod test_fixtures;
pub mod theme;
pub mod utils;
