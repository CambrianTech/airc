//! Fault-injection points for the migration and derivation tests: a test arms
//! one point on its own thread, and the code fails there with a `DbErr`, as an
//! interrupted process would. Outside tests every point is a no-op.

use sea_orm::DbErr;

/// Where a store open can be interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Migration 000021's DDL ran; its version row is not yet written.
    AfterAddressingDdl,
    /// One derivation batch committed; more rows still owe derivation.
    AfterDerivedBatch,
}

#[cfg(test)]
thread_local! {
    static ARMED: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

/// Fail at `point` once, on this thread, the next time it is reached.
#[cfg(test)]
pub(crate) fn arm(point: Fault) {
    ARMED.with(|armed| armed.set(Some(point)));
}

#[cfg(test)]
pub(crate) fn hit(point: Fault) -> Result<(), DbErr> {
    ARMED.with(|armed| {
        if armed.get() == Some(point) {
            armed.set(None);
            Err(DbErr::Custom(format!("injected fault: {point:?}")))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
pub(crate) fn hit(_point: Fault) -> Result<(), DbErr> {
    Ok(())
}
