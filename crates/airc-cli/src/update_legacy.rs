//! Retire only historical endpoints proven to serve the verified account identity.
use std::path::{Path, PathBuf};
use std::time::Duration;

fn same_owner(canonical: &airc_ipc::StatusResponse, legacy: &airc_ipc::StatusResponse) -> bool {
    let protocol = Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION));
    canonical.ipc_protocol_version == protocol
        && legacy.ipc_protocol_version == protocol
        && !canonical.peer_id.is_empty()
        && canonical.peer_id == legacy.peer_id
}

pub(crate) fn endpoint_absent(error: &airc_ipc::ClientError) -> bool {
    matches!(error, airc_ipc::ClientError::NotConnected(io)
        if matches!(io.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused))
}

pub fn verified_endpoints(
    home: &Path,
    canonical: &Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let candidates = airc_lib::socket_path::legacy_socket_paths_in(home, canonical);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let canonical = canonical.to_path_buf();
    let verified = std::thread::spawn(move || -> Result<Vec<PathBuf>, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(inspect(canonical, candidates))
    })
    .join()
    .map_err(|_| "legacy endpoint inspection thread failed")?
    .map_err(std::io::Error::other)?;
    Ok(verified)
}

async fn inspect(canonical: PathBuf, candidates: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    let status = airc_ipc::DaemonClient::new(canonical)
        .status_with_timeout(Duration::from_secs(5))
        .await
        .map_err(|error| error.to_string())?;
    let mut verified = Vec::new();
    for candidate in candidates {
        match airc_ipc::DaemonClient::new(candidate.clone())
            .status_with_timeout(Duration::from_millis(500))
            .await
        {
            Ok(legacy) if same_owner(&status, &legacy) => verified.push(candidate),
            Ok(_) => eprintln!(
                "daemon: left legacy endpoint {} untouched (identity or protocol differs)",
                candidate.display()
            ),
            Err(error) if endpoint_absent(&error) => {}
            Err(error) => {
                return Err(format!(
                    "Cannot verify legacy endpoint {}: {error}; no stop requested",
                    candidate.display()
                ))
            }
        }
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn status(peer: &str, protocol: Option<u32>) -> airc_ipc::StatusResponse {
        serde_json::from_value(
            serde_json::json!({"peer_id":peer,"uptime_seconds":1,"ipc_protocol_version":protocol}),
        )
        .unwrap()
    }
    #[test]
    fn migration_requires_verified_same_identity_and_current_protocol() {
        let version = Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION));
        let canonical = status("account-a", version);
        assert!(same_owner(&canonical, &status("account-a", version)));
        assert!(!same_owner(&canonical, &status("foreign-account", version)));
        assert!(!same_owner(&canonical, &status("account-a", None)));
        assert!(!same_owner(&canonical, &status("account-a", Some(0))));
        assert!(!same_owner(&status("", version), &status("", version)));
    }

    #[test]
    fn uncertain_inspection_is_not_an_absent_endpoint() {
        use airc_ipc::ClientError;
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::ConnectionRefused,
        ] {
            assert!(endpoint_absent(&ClientError::NotConnected(
                std::io::Error::from(kind)
            )));
        }
        assert!(!endpoint_absent(&ClientError::Timeout));
        assert!(!endpoint_absent(&ClientError::NotConnected(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied)
        )));
        assert!(!endpoint_absent(&ClientError::Io(std::io::Error::from(
            std::io::ErrorKind::UnexpectedEof
        ))));
    }

    #[tokio::test]
    async fn actual_endpoints_select_only_same_owner_and_leave_foreign_unselected() {
        use airc_ipc::{
            codec::{read_frame, write_frame},
            transport::IpcListener,
            Request, Response,
        };
        let root = tempfile::tempdir().unwrap();
        let canonical = root.path().join("canonical.sock");
        let legacy = root.path().join("legacy.sock");
        let foreign = root.path().join("foreign.sock");
        let version = Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION));
        let mut responders = Vec::new();
        for (path, peer) in [
            (&canonical, "owner"),
            (&legacy, "owner"),
            (&foreign, "other"),
        ] {
            let listener = IpcListener::bind(path).await.unwrap();
            let response = status(peer, version);
            responders.push(tokio::spawn(async move {
                let mut stream = listener.accept().await.unwrap();
                let request: Request = read_frame(&mut stream).await.unwrap().unwrap();
                assert!(matches!(request, Request::Status));
                write_frame(&mut stream, &Response::Status(response))
                    .await
                    .unwrap();
            }));
        }
        let selected = inspect(
            canonical,
            vec![legacy.clone(), foreign, root.path().join("missing.sock")],
        )
        .await
        .unwrap();
        assert_eq!(selected, vec![legacy]);
        for responder in responders {
            responder.await.unwrap();
        }
    }
}
