//! THE one owner of the threads and tokio runtimes the CLI runs async work on.
//!
//! Before this module the CLI made them in three places, each with its own builder:
//! `main`'s multi-thread runtime behind a Windows big-stack main thread, and two
//! hand-rolled "run this future on a fresh thread with its own runtime" bridges
//! (`update_legacy`, `update_shutdown`). A thread policy (a stack size, a name) could only
//! be set at each site, so it never was. Joel, 2026-10-18: written once, not per call site.
//! A new place that needs async work from sync code calls [`block_on_fresh`]; it never
//! builds a runtime of its own.

use std::process::ExitCode;

/// The Windows main thread's stack. A Windows main thread defaults to 1 MiB; the rewrite
/// set this in c088198 with no recorded measurement. Measuring it, and whether worker
/// threads need the same, is card 81d11df6's next step: its test diagnostics now name a
/// stack overflow (0xC00000FD) when one happens.
#[cfg(windows)]
const WINDOWS_MAIN_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Run the CLI's entry point on the thread it needs: on Windows a named thread with
/// [`WINDOWS_MAIN_STACK_BYTES`], elsewhere the process main thread.
pub(crate) fn run_entry(entry: fn() -> ExitCode) -> ExitCode {
    // Clippy on Windows flags an early `return` here as unneeded, and it is right: with
    // the cfg-split, each branch is the function's tail expression.
    #[cfg(windows)]
    {
        match std::thread::Builder::new()
            .name("airc-main".to_string())
            .stack_size(WINDOWS_MAIN_STACK_BYTES)
            .spawn(entry)
        {
            Ok(handle) => match handle.join() {
                Ok(code) => code,
                Err(_) => {
                    eprintln!("airc: main thread panicked");
                    ExitCode::FAILURE
                }
            },
            Err(error) => {
                eprintln!("airc: failed to start main thread: {error}");
                ExitCode::FAILURE
            }
        }
    }

    #[cfg(not(windows))]
    entry()
}

/// The CLI's process runtime: multi-thread, every driver enabled.
pub(crate) fn process_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

/// Why [`block_on_fresh`] produced no value.
#[derive(Debug)]
pub(crate) enum BridgeError {
    /// The fresh thread's runtime could not be built.
    Runtime(std::io::Error),
    /// The fresh thread panicked before the future finished.
    Panicked,
}

impl BridgeError {
    /// As an `io::Error` naming the work that failed, for callers whose errors are I/O.
    pub(crate) fn into_io(self, work: &str) -> std::io::Error {
        match self {
            Self::Runtime(error) => error,
            Self::Panicked => std::io::Error::other(format!("{work}: its thread panicked")),
        }
    }
}

/// Run `work` to completion on a fresh thread with its own current-thread runtime, and wait.
///
/// For synchronous code already running inside the process runtime, which must neither
/// nest `block_on` (tokio panics) nor hand the work to a public command that would
/// reacquire a lock it holds (the lifecycle boundaries in `update_shutdown`).
pub(crate) fn block_on_fresh<T: Send + 'static>(
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T, BridgeError> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|runtime| runtime.block_on(work))
    })
    .join()
    .map_err(|_| BridgeError::Panicked)?
    .map_err(BridgeError::Runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the bridge losing the value, running the work on the caller's
    // runtime (a nested block_on panics), or swallowing a panic into a success.
    #[test]
    fn block_on_fresh_runs_inside_a_runtime_and_reports_a_panic() {
        let runtime = process_runtime().expect("runtime builds");
        let value = runtime.block_on(async { block_on_fresh(async { 41 + 1 }) });
        assert_eq!(value.expect("the bridge returns its value"), 42);
        let panicked = block_on_fresh(async { panic!("boom") });
        assert!(matches!(panicked, Err(BridgeError::Panicked)));
        assert!(panicked
            .unwrap_err()
            .into_io("probe")
            .to_string()
            .contains("probe: its thread panicked"));
    }
}
