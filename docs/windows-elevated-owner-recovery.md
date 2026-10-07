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

## Public entry token policy

Run full Windows installation and explicit recovery from a normal user terminal.
An already elevated full entry now refuses before helper acquisition, source
checkout, build, or installation writes. Setup owns the narrow consent session
for firewall and recovery work; it never adopts a daemon under an administrator
token. Explicit firewall-only and read-only diagnostic operations remain usable
from an elevated observer. Both PowerShell and Windows Bash entries project the
same canonical token check from the pinned shared helper.

Hosted Windows acceptance creates a disposable standard account and starts an actual
medium-integrity process with its credentials. A native named-pipe observation supplies its PID and
token, a retained process handle and birth time protect that observation, and
ancestry must lead back to the test supervisor before managed gsudo process-scoped consent is
granted. The unchanged public installer runs in that child, followed by doctor
and a native check that its daemon is also normal-token. The disposable runner
cleans up its owned daemon and scoped cache. No token-changing acceptance test
is run on a user's desktop.

Local token-policy, source-acquisition and PATH fixtures pass; the hosted real
medium-token installation remains OPEN until its CI receipt is available. A
runner that cannot provide a real normal token fails explicitly; tests never
mock normality for full installation.

The built-in Administrator (RID 500) remained elevated even after gsudo lowered
its integrity to Medium in the first hosted test. The fixture correctly refused
that token before installation. The replacement uses a real disposable standard
account; it also requires captured-child cancellation to close its owned native
descendant before attempting full installation. Account/profile cleanup waits
for the exact test user's processes to exit and validates the profile path.
# Installer scope is independent of the invoking checkout

Public installation selects `AIRC_HOME` when explicitly set, otherwise the
invoking account's `HOME/.airc` (`USERPROFILE` fallback on native Windows).
It passes that explicit home to both endpoint resolution and installed-daemon
adoption. Running setup beneath another account's `.airc/worktrees` must never
select that ancestor or open its event database. Ordinary CLI project scope,
`--home`, and `--here` behavior remains unchanged. Bash-to-Windows paths use the
existing `cygpath` adapter before the PowerShell adoption entry.

The `installer_scope` binary regression executes the public adoption adapter
from a foreign `.airc/worktrees` directory with isolated `HOME`, then repeats
with explicit `AIRC_HOME`; it checks the selected database/socket, live daemon,
and unchanged foreign database. Hosted Linux/macOS execute the actual extracted
POSIX callsite; Windows extracts the actual scope/adoption callsite from
`windows/adopt-installed.ps1` (full normal-token entry is exercised separately
by the hosted standard-account public installation).

## Legacy backup labels and shutdown acknowledgements

The legacy updater labeled airc.old-<revision> using its source checkout, which
can differ from the displaced running binary. The filename remains validated
as an owned backup under the installed directory, but its label is not compared
to Status.build_commit. The held-pipe PID, same-SID high token, executable path,
canonical protocol and valid observed runtime revision remain required.

Some old daemons close their connection while exiting before the Stop reply is
read. After a successful canonical Stop write, only an acknowledgement or
shutdown-compatible EOF may proceed to the existing bounded wait on the held
verified process handle. EOF while the process lives, explicit protocol errors,
malformed replies, other I/O failures and timeout remain failures. No force
termination is introduced. Native isolated child/IPC regressions cover these
outcomes; live elevated recovery remains a separate installation receipt.
