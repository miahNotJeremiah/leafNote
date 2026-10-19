//! # leafnote-core
//!
//! The pure-Rust heart of **leafNote**, a 1:1 open-source, *local-first* clone
//! of Microsoft OneNote.
//!
//! This crate has **no GUI dependencies** so it can be reused by the desktop
//! app (`leafnote-app`), headless tooling, tests, and future bindings.
//!
//! ## Modules
//! - [`model`] — the notebook / section-group / section / page document model
//!   with rich text runs, ink strokes, images, and file attachments.
//! - [`store`] — local-first persistence. Everything lives in a plain folder
//!   on your disk (one JSON file per page + a `tree.json` index + content-addressed media). No account, no cloud.
//! - [`import`] — native **read-only** support for real OneNote files built on
//!   top of the `onenote_parser` crate: `.one` sections, `.onetoc2` tables of
//!   contents, and `.onepkg` package archives (converted into the leafNote
//!   model without ever touching the original files).
//! - [`search`] — full-text search across titles, body text, tags, and section names.
//! - [`canvas`] — geometry helpers for the infinite freeform canvas (stroke
//!   bounding boxes, hit-testing, smoothing).
//! - [`config`] — application configuration & default data directory.

pub mod canvas;
pub mod config;
pub mod import;
pub mod model;
pub mod search;
pub mod store;

pub use error::CoreError;

mod error {
    /// Unified error type for all core operations.
    #[derive(Debug, thiserror::Error)]
    pub enum CoreError {
        #[error("I/O error: {0}")]
        Io(#[from] std::io::Error),
        #[error("serialization error: {0}")]
        Serde(#[from] serde_json::Error),
        #[error("TOML error: {0}")]
        Toml(#[from] toml::de::Error),
        #[error("OneNote parse error: {0}")]
        OneNote(String),
        #[error("not found: {0}")]
        NotFound(String),
        #[error("{0}")]
        Other(String),
    }

    pub type Result<T> = std::result::Result<T, CoreError>;
}

/// Re-export of the core result alias.
pub type Result<T> = error::Result<T>;

/// Current on-disk format version of leafNote notebooks.
pub const FORMAT_VERSION: u32 = 1;
