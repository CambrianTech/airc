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

2026-10-01 follow-up receipts: PR1470 head d547fd4a now makes the exact Cargo
exit42 reproduction return42 with an explicit error; isolated probe verifies
source repair only, actual installer path acceptance still OPEN. PRcomment
5935774138 records the result. Registry shutdown PR1471 merged as d249ac471111;
normal public airc update exited0, built release in38.76s, installed the verified
artifact and automatically restarted the daemon. CLI/daemon both d249ac471111,
same peer identity. This run needed no retry/rejoin, but did not deliberately
exercise registry rate limiting during shutdown, so that live case remains OPEN.
SOS separate-reader repair PR1472 head9ee8c351 has10 focused tests passing and
independent APPROVE; CI/install pending. Upgrade may replay history once through
the new join cursor; same-type readers still share a cursor.

2026-10-01 independent firewall/outer-installer review: exact PR1470 d547fd4a
policy fixture passes Windows PowerShell5.1 in isolated extracted files, with no
host firewall changes. Continuum17c2096f Mod-AircFirewall (win-modules.ps1:437)
still adds a separate broad program rule and skips on name existence after
Mod-Airc; borrowing elevation alone does not consolidate policy ownership.
Shared policy verification must be delegated to AIRC; standalone lifecycle must
remain possible. install-common owns Ensure-Elevated/Invoke-Elevated/Clear-Elevation
and the outer finally disposes its cache, so nested borrowed sessions must not
clear it. Exact finding/fixture limits recorded on PR1470. No consumer edits or
installer rerun; integration and live policy acceptance remain OPEN.

2026-10-01 installed-reader adoption: PR1472 all CI green; exact9ee8c351 merged
as5f76f3d06952. Normal public update exited0 in44.04s and automatically restarted
daemon; CLI and daemon match5f76f3d with unchanged peer identity. Three previous
join clients (verified PIDs26560,7992,3576 and exact join command lines) survived
binary updates with old code. Stopped only those owned join clients and started
one current public join cambriantech; daemon was preserved. This manual client
refresh is an OPEN update-consumer lifecycle case: installer/runtime must make
existing reader adoption automatic with regression/end-to-end proof before
claiming complete update recovery. Current join owner session85064/PID29912.
No BIGGIEDESK installation or mesh acceptance inferred.

2026-10-01 installed SOS acceptance: foreground watch and background join85064
both delivered BIGGIEDESK's same PR4649/PR1470 checkpoint; separate cursor files
exist. This closes the observed cross-reader consumption reproduction locally,
not the broader automatic client upgrade case or peer mesh onboarding.

Independent PR4649 extraction review at90afe17b: all six function AST extents
match actual canary baseb2849a6c after whitespace/logging substitutions. Fixture
changes preserve nonzero error results and refusal assertions. Extraction-only
APPROVE subject CI; no full-suite rerun or elevation performed by this reviewer.
Borrowed-cache lifetime and standalone AIRC consumer integration remain OPEN,
owned by BIGGIEDESK. Review comment5936259507 in Continuum records exact limits.
An initial comparison used origin/main rather than the PR's canary base and
failed before producing evidence; corrected to the API-reported base above.

2026-10-01T17:12Z BIGGIEDESK live peer projection investigation (OPEN):
- Peer reported daemon0121d959, project author d4ad790b, peers0 versus publish17.
  Both owners independently traced IDs to intentional machine/project scopes;
  no identity corruption established and no keys/scopes changed.
- Ordinary event08c65378 received here; nonce echoed only on ordinary AIRC as
  eventa6eea9a2. Reverse receipt remains unproved; no SOS echo used as acceptance.
- Source root cause for misleading peer list: run_peer_list loaded only scope
  while Airc::peers loads scope plus machine. Codex added shared library
  peer_trust_snapshot and thin CLI consumer, preserving full trust metadata,
  stable scope precedence, existing JSON shape, and local enrollment visibility.
- Regression proves machine-only enrollment visible from empty project scope,
  deduplication, scope key precedence, same-home handling and absence of both
  identity.key and SQLite local identity. PASS. cargo check airc-cli PASS.
  Two initial compile errors (PeerId ordering; test DB constant/borrow) corrected;
  focused test rerun PASS. Independent review APPROVE; human footer corrected.
- This is source/test evidence only. Installer adoption and BIGGIEDESK normal
  peers command acceptance remain OPEN. No daemon restart/manual pairing.
