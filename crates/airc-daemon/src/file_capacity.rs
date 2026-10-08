//! Daemon-owned descriptor capacity, independent of the launching shell.
//! A Mac SSH session inherited 256 despite a live mesh needing 703 sockets.
//! Reserve 4096 descriptors where the existing hard limit permits it. This
//! allocates no descriptors, never lowers a limit, and does not promise that
//! arbitrary mesh growth fits. Restricted hosts continue with truthful warning.
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
    StderrJsonDiagnosticSink,
};
use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};

const DESIRED: u64 = 4096;

fn target(limit: Rlimit) -> Rlimit {
    let Some(current) = limit.current else {
        return limit;
    };
    Rlimit {
        current: Some(current.max(DESIRED.min(limit.maximum.unwrap_or(DESIRED)))),
        ..limit
    }
}

fn display(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unlimited".into())
}

pub(crate) fn prepare() {
    let before = getrlimit(Resource::Nofile);
    let desired = target(before);
    let error = if desired != before {
        setrlimit(Resource::Nofile, desired).err()
    } else {
        None
    };
    let after = getrlimit(Resource::Nofile);
    let constrained = after.current.is_some_and(|current| current < DESIRED);
    let mut event = DiagnosticEvent::new(
        if constrained || error.is_some() { DiagnosticSeverity::Warn } else { DiagnosticSeverity::Info },
        DiagnosticComponent::Daemon, DiagnosticCode::DaemonFileCapacity,
        if constrained { "Daemon descriptor capacity remains below the startup target; mesh growth may exhaust it" }
        else { "Daemon descriptor capacity observed at startup" },
    ).with_field("before_soft", display(before.current))
     .with_field("actual_soft", display(after.current))
     .with_field("hard", display(after.maximum))
     .with_field("desired_soft", DESIRED);
    if let Some(error) = error {
        event = event.with_field("error", error);
    }
    StderrJsonDiagnosticSink.emit(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_lowers_limits_or_exceeds_declared_hard_limit() {
        for (soft, hard, expected) in [
            (Some(256), None, Some(4096)),
            (Some(256), Some(1024), Some(1024)),
            (Some(128), Some(128), Some(128)),
            (Some(8192), Some(16384), Some(8192)),
            (None, None, None),
        ] {
            assert_eq!(
                target(Rlimit {
                    current: soft,
                    maximum: hard
                }),
                Rlimit {
                    current: expected,
                    maximum: hard
                }
            );
        }
    }

    #[test]
    fn unix_child_recovers_inherited_low_soft_limit() {
        const CHILD: &str = "AIRC_FILE_CAPACITY_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let original = getrlimit(Resource::Nofile);
            let low = 256.min(original.maximum.unwrap_or(256));
            setrlimit(
                Resource::Nofile,
                Rlimit {
                    current: Some(low),
                    ..original
                },
            )
            .unwrap();
            prepare();
            let after = getrlimit(Resource::Nofile);
            assert_eq!(
                after,
                target(Rlimit {
                    current: Some(low),
                    ..original
                })
            );
            return;
        }
        // Isolate process-global limits from the parallel test runner.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_capacity::tests::unix_child_recovers_inherited_low_soft_limit",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("daemon_file_capacity"));
    }
}
