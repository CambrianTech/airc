# Elevated daemon recovery in public setup

An old updater could leave a high-integrity daemon running from a renamed
`airc.old-<revision>` file. A normal-token client then receives Windows access
denied. The public Windows installer now probes the selected native endpoint
before adoption. Only typed access denied enters the existing owned elevation
session; unrelated observation failures remain failures.

The recovery CLI holds the connected pipe and its actual server process handle.
It requires matching original-caller, observer and owner SIDs; an elevated
high-integrity token; and an image within the installed AIRC destination or its
known updater backup forms. A second PID-bound connection verifies canonical
protocol/build status; a legacy backup suffix must match that runtime revision.
The original held connection sends canonical `Request::Stop`, and the captured
process handle must signal exit. There is no force kill, PID-file ownership,
ACL adjustment or elevated daemon launch. Token/PID evidence is rechecked before
Stop. Normal-token adoption runs only after successful recovery.

For an explicitly diagnosed existing installation, `install.ps1
-RecoverElevatedDaemon -AircPath <installed-airc.exe>` uses the same boundary.
The executable must already be published at the canonical installed destination;
a candidate in a build target directory is not that destination. Prefer running
the public installer from the reviewed candidate checkout, which publishes the
new executable before its automatic recovery/adoption step.

Observed original failure: Bigmama pipe owner PID 25236, same user SID, elevated
token, integrity 12288, image `airc.old-8eaa592`. Historical rename provenance is
the updater before commit cc5aadb. Local tests exercise refusal against a real
isolated pipe owner, native token queries, path/identity guards, and synthetic
PS5 public adoption/elevation sequencing. Positive elevated-owner recovery and
live account adoption remain OPEN until the supported installer is exercised.
