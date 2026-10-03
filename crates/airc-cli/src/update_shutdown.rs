//! Observe and stop the lifecycle owner's daemon without starting or terminating
//! a process. Callers hold lifecycle ownership through shutdown completion.
//! Windows must release the mapped executable before maintenance can proceed.
use std::path::Path;
use std::time::{Duration, Instant};

/// A failed observation is not evidence that the owner is absent. In particular,
/// AccessDenied on a Windows pipe must not authorize a second owner or success.
pub(crate) fn status(
    socket: &Path,
) -> Result<Option<airc_ipc::response::StatusResponse>, Box<dyn std::error::Error>> {
    let endpoint = socket.to_path_buf();
    let result =
        daemon_rpc(async move { airc_ipc::client::DaemonClient::new(endpoint).status().await });
    classify_daemon_status(result).map_err(|error| {
        format!("Cannot establish daemon state at {}: {error}; refusing to treat an unknown owner as absent", socket.display()).into()
    })
}

/// Synchronous CLI lifecycle boundaries are called inside Tokio. Keep typed IPC
/// calls on a short-lived runtime rather than nesting block_on or spawning a
/// public lifecycle command that would reacquire maintenance.
fn daemon_rpc<T: Send + 'static>(
    operation: impl std::future::Future<Output = Result<T, airc_ipc::client::ClientError>>
        + Send
        + 'static,
) -> Result<T, airc_ipc::client::ClientError> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(airc_ipc::client::ClientError::Io)?;
        runtime.block_on(operation)
    })
    .join()
    .map_err(|_| {
        airc_ipc::client::ClientError::Io(std::io::Error::other("daemon IPC thread failed"))
    })?
}

fn classify_daemon_status(
    result: Result<airc_ipc::response::StatusResponse, airc_ipc::client::ClientError>,
) -> Result<Option<airc_ipc::response::StatusResponse>, airc_ipc::client::ClientError> {
    match result {
        Ok(status) => Ok(Some(status)),
        Err(error) if crate::update_legacy::endpoint_absent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Request graceful shutdown and confirm completion under the caller's existing
/// lifecycle guard. This never changes durable operator intent: public stop
/// records it before calling, while updater shutdown remains transient.
pub(crate) fn stop(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if status(socket)?.is_none() {
        return Ok(());
    }
    // Bind the wait handle to this endpoint before stop. A shared informational
    // PID file can refer to a legacy daemon listening on a different endpoint.
    // A failed capture after observing an owner is an error, not proof of exit.
    #[cfg(windows)]
    let exiting = {
        let endpoint = socket.to_path_buf();
        daemon_rpc(async move {
            let connection = airc_ipc::transport::IpcStream::connect(&endpoint)
                .await
                .map_err(airc_ipc::client::ClientError::NotConnected)?;
            let pid = connection
                .server_process_id()
                .map_err(airc_ipc::client::ClientError::Io)?;
            DaemonExit::capture_process(pid).map_err(airc_ipc::client::ClientError::Io)
        })?
    };
    let endpoint = socket.to_path_buf();
    let stop_result =
        daemon_rpc(async move { airc_ipc::client::DaemonClient::new(endpoint).stop().await });
    #[cfg(not(windows))]
    stop_result?;
    #[cfg(windows)]
    {
        // Shutdown can close IPC before delivering its reply. Only the pinned
        // process's confirmed exit permits proceeding after a failed response.
        exiting.wait(Duration::from_secs(20))?;
        if let Err(error) = stop_result {
            eprintln!("Stop response failed ({error}), but the original daemon has exited.");
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while status(socket)?.is_some() {
        if Instant::now() >= deadline {
            return Err("daemon still answers after stop; shutdown not confirmed".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(windows)]
mod process_exit;
#[cfg(windows)]
pub(crate) use process_exit::DaemonExit;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_missing_or_refused_connection_is_absence() {
        use airc_ipc::client::ClientError;
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::NotFound, ErrorKind::ConnectionRefused] {
            assert!(
                classify_daemon_status(Err(ClientError::NotConnected(Error::from(kind))))
                    .unwrap()
                    .is_none()
            );
            assert!(
                classify_daemon_status(Err(ClientError::Io(Error::from(kind)))).is_err(),
                "a connection that failed after opening is not absence"
            );
        }
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::TimedOut,
            ErrorKind::WouldBlock,
            ErrorKind::ConnectionReset,
            ErrorKind::Other,
        ] {
            assert!(
                classify_daemon_status(Err(ClientError::NotConnected(Error::from(kind)))).is_err()
            );
        }
        assert!(classify_daemon_status(Err(ClientError::Timeout)).is_err());
        assert!(
            classify_daemon_status(Err(ClientError::UnexpectedResponse(Box::new(
                airc_ipc::response::Response::Pong
            ))))
            .is_err()
        );
    }
}
