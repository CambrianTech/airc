//! ONE lock for tests that mutate or read process environment the code under
//! test derives paths from (`HOME` → `machine_account_home`). `cargo test` runs
//! tests in parallel threads of one process; a test that sets `HOME` while
//! another derives a home from it makes the reader wrong — measured 2026-09-15:
//! `daemon_command_spawns_the_owning_scope_never_the_caller_scope` failed 1 run
//! in 4 against `identity_card_home_routes_…`'s `set_var("HOME")`. Hold this in
//! both kinds of test; a poisoned lock (a panicking test) is still a lock.

pub(crate) static HOME_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn home_env_guard() -> std::sync::MutexGuard<'static, ()> {
    HOME_ENV
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
