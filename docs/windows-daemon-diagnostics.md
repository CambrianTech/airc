# Read-only Windows daemon diagnostics

When Windows denies daemon IPC, the daemon state is **unknown**, not stopped.
Do not replace it, change its permissions, or infer its token from a PID file or
scheduled task registration. A task configured as Interactive/Limited does not
prove which token an already-running process holds.

From a current AIRC checkout, the supported installer diagnostic mode is:

```powershell
.\install.ps1 -DiagnoseDaemon
```

For a nondefault installation, supply `-AircPath` with the installed executable.
This mode requires an installed build supporting `ipc-endpoint --native`. An
older binary fails clearly before requesting consent. Endpoint resolution runs
under the original account and environment; the installer does not reproduce
the transport's endpoint hash or select a different scope after elevation.

The existing installer elevation session requests Windows consent once to run
the read-only observer. No prerequisite installation, build, source update,
firewall policy, startup task, daemon stop, or permission repair follows this
mode. Normal installation remains a separate invocation.

The JSON receipt contains the selected endpoint, its OS-reported server PID,
image path, token user SID, token elevation boolean and token integrity SID,
plus the original caller and observer SIDs. The observer opens the selected
pipe without exchanging protocol messages, keeps that handle during process
inspection, and requests only process limited-query and token query access.
It does not enable debug privileges.

Any unobservable field remains `UNKNOWN` with its Win32 error. In particular,
pipe-open error 5 is not evidence that the daemon is elevated, and success of
this diagnostic is not evidence that ordinary IPC or peer messaging works.
The report authorizes no automatic repair. No running build SHA is inferred
from an installed binary or its filename.

Regression coverage uses unique synthetic named pipes, including an actual
hidden PS5 child observer and an access-denied pipe. It does not probe a live
daemon, change live ACLs, request UAC, or open a LAN listener.
