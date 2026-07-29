//! Scanner + config as a library, so the CLI works without the daemon running.

pub mod config;
pub mod docs;
pub mod scan;
pub mod search;

pub use config::Config;
pub use docs::{list_docs, read_doc, write_doc};
pub use scan::{is_ignored, scan_all, scan_project, SKIP_DIRS};
