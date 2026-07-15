//! Compatibility surface for the legacy `ploy` package name.
//!
//! Runtime ownership now lives in dedicated workspace applications such as
//! `ployd` and `ployctl`. This crate intentionally stays small so the root
//! package no longer acts as the platform composition root.

pub const COMPATIBILITY_NOTICE: &str =
    "The `ploy` crate is a compatibility shim. Use `ployd` or `ployctl` for workspace entrypoints.";

pub const WORKSPACE_APPS: &[&str] = &["ployd", "ployctl"];

pub fn compatibility_notice() -> &'static str {
    COMPATIBILITY_NOTICE
}

pub fn workspace_apps() -> &'static [&'static str] {
    WORKSPACE_APPS
}
