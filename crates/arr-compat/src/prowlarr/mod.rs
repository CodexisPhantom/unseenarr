//! The Prowlarr v1 API, read-only, for importing indexers
//! (`docs/08-integrations.md`, "Prowlarr as import source", and
//! `docs/14-compatibility.md`, section C).

pub mod client;
pub mod dto;

pub use client::{ProwlarrClient, ProwlarrError, feed_url};
