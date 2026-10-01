# BIGGIEDESK fresh installation: open acceptance cases

Owner: BIGGIEDESK Codex for installer changes; BIGMAMA Codex for SOS and independent review.
All cases remain OPEN until the normal installation exercises the repair end to end.
This is the set known to BIGMAMA, not a claim that BIGGIEDESK's intervention inventory is complete.

| Case | Observed intervention or investigation | Repository repair and regression | End-to-end status |
| --- | --- | --- | --- |
| SOS room collision | BIGMAMA ran `airc join sos`; it selected a mesh room while the other machine used the recovery gist. | CLI rejects reserved names before bootstrap and resolved default/name/UUID writes; two regression tests in commands.rs. | BIGMAMA installed10e5a147; live refusal verified. BIGGIEDESK remains OPEN. |
| Recovery starvation | Repeated `airc sos watch/send` refused at30/30 despite nominal SOS reserve. Source investigation found conflicting CLI/library reservation policies. | CLI uses library GhBudget; explicit Recovery class; shared-budget saturation/window/backoff regression in governor.rs. | BIGMAMA CLI/daemon adopted10e5a147; SOS read/send succeeded. Live saturation and BIGGIEDESK remain OPEN. |
| Update shutdown handoff | Normal BIGMAMA update built in1m43, then failed after daemon acknowledged stop but did not exit within20s. Later IPC and daemon process were absent. Re-ran normal update after confirming no active build/daemon, then explicitly joined cambriantech. | Investigate shutdown completion and automatic resumption using existing update ownership; current failure discards the temporary prepared artifact and returns before restoring service. | OPEN: retry installed10e5a147 and explicit join restored same identity; manual recovery is not a lifecycle fix. |
| Fresh-toolchain compilation | Local Rust1.96 checks passed; clean CI Rust1.99 rejects async-trait0.1.89 generated must_use attributes. Inspected CI log and dependency macro source. | Cargo.lock upgrades only async-trait to0.1.92, whose upstream release fixes this diagnostic. Existing full CI retains deny-warnings. | OPEN: local clippy passes; fresh CI and installer build still required. |
| Fresh-toolchain doctor lint | After the macro upgrade, clean CI reaches doctor/delivery.rs and rejects the unnecessary borrow in `last.map(&age)`. | Pass the copyable closure directly with `last.map(age)`; keep the existing deny-warnings check. | OPEN: local clippy passes; fresh CI and installer execution pending. |
| Build Tools disk failure | BIGGIEDESK reported VS exit0x80070070, C:5.6GB free and D:4.2TB; installer gave generic Modify advice. | BIGGIEDESK owns installer error classification and supported volume planning; reviewable patch/test receipt pending. | OPEN: no successful supported install receipt. |
| Cargo output on another volume | Static review plus `cargo metadata --format-version 1 --no-deps` with CARGO_TARGET_DIR=D:/continuum-cold/cargo-target confirms Cargo output differs from install.ps1's hardcoded source-local target/release/airc.exe. | BIGGIEDESK installer owner notified: resolve actual artifact from Cargo target_directory rather than guessing the build location; exercise nondefault target through installer. | OPEN: source fix and end-to-end receipt pending; successful compilation alone can still fail installation or select stale output. |
| Invisible consent/auth | BIGGIEDESK reported gh MSI hidden UAC; gh authentication later completed manually. | BIGGIEDESK must inventory exact interventions and encode required detection, visible consent, waiting and resumption in existing installer modules. | OPEN: preexisting authenticated state is not proof this path works. |

Upstream compiler compatibility evidence: [async-trait0.1.92 release](https://github.com/dtolnay/async-trait/releases/tag/0.1.92).

For every additional intervention, record the command/action, failure, cause,
installer implementation, regression and actual end-to-end receipt. Label
prerequisites skipped because they were manually installed. Do not erase working
state merely to manufacture a clean-install claim. SOS acknowledgements do not
prove the AIRC mesh is installed or connected.

Investigation ruled out: Windows CI stages with `Copy-Item -Recurse -Force *`.
A temporary Windows fixture with a hidden .git directory confirmed that both
.git and install.ps1 are copied. No staging repair is justified by that
hypothesis. The existing Windows CI retains this staging step; its install
result is still required. Fixture did not run an installer or modify real state.

BIGMAMA adoption receipt: normal update retry installed10e5a147; CLI and daemon
status agree on10e5a147bb34 with unchanged peer e85a5bb3-74f0-4325-87df-7d5f27637063.
Installed `airc join sos` refuses mesh routing before changing the room.
Shutdown handoff required operator retry/rejoin, so lifecycle acceptance remains
OPEN. This is not BIGGIEDESK onboarding acceptance or a live saturation proof.

CI receipt for PR1469 head34829dc: Windows install.ps1 job110438597755
passed in10m1s; PowerShell5.1 job110438597949 passed in8m22s. The Windows
log explicitly reports GitHub CLI, Rust and MSVC build tools already installed.
It built and installed airc.exe and doctor returned eight clean checks, with
no identity in the fresh scope. This verifies installation with existing
prerequisites; it does NOT exercise BIGGIEDESK's missing Build Tools, low-disk,
UAC/auth bootstrap or two-peer onboarding cases. Those cases remain OPEN.

2026-10-01 shutdown investigation (Codex): registry_refresh::run_loop awaited
honor(RateLimited) inside a selected tick arm, preventing shutdown observation
through the entire governor retry window (60 seconds can exceed updater's
20-second process-exit deadline). Repair keeps admitted registry writes intact
and selects shutdown while honoring backoff. A real-loop denied-store regression
proves prompt exit without a second publish; existing backoff tests retain the
full retry window absent shutdown. All eight registry-refresh tests pass locally.
This independently confirmed defect is consistent with the observed update
failure; the original process did not retain enough phase evidence to prove it
was its only cause. In-flight GitHub calls and runtime teardown remain separate
possible delays. No process-exit gate weakened, forced termination introduced,
or install acceptance claimed. Normal update end-to-end validation remains OPEN.

2026-10-01 independent PR1470 review (Codex/BIGMAMA), exact head
 eeffd5ac621c26358eaf9c2066013877f03c8c16: moving Windows into install.sh
removes its duplicated hardcoded artifact location, but the shared
_airc_target_dir still guesses source/target after failed metadata. Extracted
that exact function into an isolated Git Bash fixture, enabled set -euo pipefail,
and replaced cargo with an exit42 stub: resolver_exit=0 resolved=/tmp/target.
No installation or real artifact selection occurred. BLOCK finding posted at
https://github.com/CambrianTech/airc/pull/1470#issuecomment-5935531306 and canonical
SOS; installer owner retains repair. Required: explicit metadata/parse failure,
configured-target success coverage, then actual shared installer acceptance.
OPEN; reviewer did not mutate the peer branch or start duplicate installation.

2026-10-01 SOS delivery investigation: foreground sos watch repeatedly reported
no new peer messages, but the redirected, still-active public join log contained
BIGGIEDESK's PR1470 announcement and standalone-AIRC requirement. Source proved
poll_fallback_once and run_watch shared sos-watch-cursor. Background printing
therefore acknowledged a message on behalf of a different foreground consumer.
Repair separates the join cursor from watch, reusing filtering and delivery
logic; regression verifies both consumers get a peer comment once and filter
self comments. Existing watch cursor is preserved; messages consumed before the
repair are not retroactively recovered by it. The observed message was recovered
read-only from the join log and acknowledged via SOS. Local regression/adoption
receipts pending; live installed delivery acceptance remains OPEN.
