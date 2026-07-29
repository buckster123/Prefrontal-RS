//! Scanner + config as a library, so the CLI works without the daemon running.

pub mod config;
pub mod scan;

pub use config::Config;
pub use scan::scan_all;
