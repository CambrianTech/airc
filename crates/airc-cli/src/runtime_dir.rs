//! The airc runtime directory. MOVED to `airc-lib` so non-binary callers can reach it —
//! see [`airc_lib::socket_path`] for why a binary-only home was the defect.
//!
//! Re-exported here so airc-cli's existing call sites keep working unchanged.

pub use airc_lib::runtime_dir::runtime_dir;
