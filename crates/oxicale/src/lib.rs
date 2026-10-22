//! Oxicale: a small, self-hosted CalDAV and CardDAV server.
#![allow(clippy::result_large_err)]

pub mod auth;
pub mod cli;
pub mod config;
pub mod crypto;
pub mod dav;
pub mod feeds;
pub mod import;
pub mod rights;
pub mod server;
pub mod sessions;
pub mod subscriptions;
pub mod web;
pub mod xml;
