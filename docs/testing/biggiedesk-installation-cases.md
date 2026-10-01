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

2026-10-01T17:14Z independent ownership review of Continuum4649 exact78cf0366b:
reviewed PID/start-time/ancestry validation, native gsudo selection, preservation
of existing caches, parent-only cleanup, and installer finally wiring. PS5.1
fixture against exact helper passed borrower cleanup, owner context cleanup and
stale-owner refusal; discovery stubbed, no helper/elevation/installation invoked.
Production child ancestry tests inspected, not rerun here. APPROVE ownership
slice subject CI; artifact acquisition, exact AIRC adapter, canonical firewall
integration and fresh-install E2E remain OPEN with BIGGIEDESK owner. Source
retrieval initially used a nonexistent install-elevation filename (404); corrected
to PR-listed windows-elevation.ps1. No production files or runtime modified.

2026-10-01T17:23Z PR1473 CI caught a regression on Windows/Linux/macOS:
project initialization enrolls its own identity in the machine trust store, so
naively exposing that union inflated peer counts and displaced expected rows.
No failed head was merged or installed. Repaired shared snapshot to exclude
persisted default scope/machine identities without minting identities. Added
actual initialized-scope regression; both focused library tests PASS. Existing
CLI peer_commands integration suite is the acceptance regression for the changed
public command (running, six of seven passed at this checkpoint). Source-only;
BIGGIEDESK ordinary return ACK and normal installer adoption remain OPEN.
Follow-up review caught the supported named-agent self-enrollment variant.
Snapshot now reuses airc-identity requested_agent_name and excludes the active
scope row plus machine default; other named citizens remain visible. Three
library regressions PASS and final existing CLI peer_commands suite7/7 PASS.
Independent revised review APPROVE subject required checks. No failing head
installed; CI must pass on this revision before merge/adoption.

2026-10-01T17:34Z independent artifact-boundary review of peer1470 exact944440f:
independently fetched both pinned Continuum0aad524 source blobs via contents API;
SHA256 exactly matches lock (elevation8d2957a8...9155bytes,
manifest4d819027...4682bytes). Reviewed cached/downloaded integrity-before-load,
unique staging/cleanup and standalone native owner calling existing Bash stages.
No blocker in this slice, no helper execution/install. Firewall delegation and
live consent/fresh installation remain OPEN. Review posted PR1470.
Fresh doctor evidence reports BIGGIEDESK transport0121d9594468/4468 aggregate
ACKs, latest8sec/RTT26ms, while peer confirms reader lacks echoa6eea9a2. This
narrows but does not prove event-level delivery; no broad ACK acceptance claim.
Sent exact distinction to owner through SOS. No manual pairing/restarts.

2026-10-01T17:43Z independent peer1470 source review ff01350 versus944440f:
waiting subshell/source launcher preserves native owner ancestry while keeping
PSModulePath removal/exit isolated. Explicit status propagation inspected; strict
owner validation unchanged. Expanded fixture uses real prerequisite helper and
asserts Borrowed but stops before binary/firewall/startup stages. No blocking
source finding; did not rerun peer-owned fixture or live install. CI/live consent
and installation acceptance remain OPEN. Review posted on PR1470.

2026-10-01T17:54Z actual local installation acceptance of PR1473:
All required Rust and clean-install CI checks passed on a0b4cc4. Merged exact
head to canary9fdb8c1090f9. No deployment claim/compiler before normal resolved
public airc update29685. Exit0; release build47.29s, installer repaired existing
autostart and automatically restarted daemon with build verified. CLI and daemon
both9fdb8c1090f9, transportidentity e85a5bb3 preserved. Installed public peer list
from existing Continuum project scope returns138 including BIGGIEDESK0121 with
own_account tier. This closes local consumer adoption of peer-view correction;
BIGGIEDESK fresh installer acceptance remains OPEN. Most prereqs already present.
New ordinary return challenge eventab43ccdf/case biggiedesk-return-1755 sent after
update, ACK pending; nonce excluded from SOS. No manual pairing/extra daemon.
Peer1470 Windows setup-consent fails incompatible developer-source refusal test;
direct job110504677515 logs retained and exact failure sent to installer owner.
Joel requires presence for necessary clicks: told owner explicitly notify exact
machine/action and wait for actual UAC/auth; preparation is not consent/testing.

2026-10-01T17:57Z executed independent exact7c3bdb3 Windows bridge fixture
under PS5.1 in isolated copied source. PASS fresh source, stale managed source,
explicit developer refusal, path handoff, immutable artifact integrity/cache
repair and mismatch refusal. Fixture mocks package/auth/elevation and cleans its
verified TEMP root; no real install/consent. This contrasts with hosted PS5.1
job110504677515 failing developer-source refusal on same head; environment-
sensitive discrepancy unresolved, not grounds to dismiss CI. Exact evidence
sent owning peer. Review fixture retained under state/team-proof/review-bridge-
7c3bdb3. Source fetches and all setup within that isolated review tree only.

2026-10-01T18:07Z executed exact bcfea093 bridge fixture PS5.1: PASS.
Mutation check substituted old7c3bdb3 public entry into the same isolated new
fixture and ALSO PASSED. New bytes restored in finally. This fixture alone does
not distinguish the old public-entry status-capture defect locally, despite its
caller LASTEXITCODE73. Reported exact result to owner; do not treat new fixture
pass alone as proof of failure repair. Shared-helper native reproduction may
supply coverage separately; not run here. No production installer/consent.

2026-10-01T18:16Z independent failing-old/passing-new PATH regression proof:
exact b172f6e bridge fixture on PS5.1 exits0 with new public entry. Substituting
prior bcfea093 entry into SAME new fixture exits1, Environment variable name or
value is too long, at fixture line109. Restored new bytes finally. Confirms PATH
repair is exercised and distinguishes earlier status-capture hypothesis from
actual reported CI failure. No real packages/consent/profile changes; isolated
fixture only. Sent exact result via SOS. Live BIGGIEDESK acceptance remains OPEN.

2026-10-01T18:25Z peer-reported first real BIGGIEDESK public install:
Owner reports normal native install.ps1 b172f6e exited0 with Joel present and
administrator consent obtained. Existing Git/Rust/MSVC and joelteply auth reused;
new gsudo portable acquired through canonical manifest, D storage selected,
effective TCP+UDP LocalSubnet policy and startup verified. This is peer-reported
live evidence, not an independently inspected remote log or fresh-prerequisite
proof. Post-install CLI b172f6e versus daemon a3e04c6 exposed stale adoption.
No manual daemon restart: owner repaired normal installer in PR1470 aed17e9427.

Independent source review of exact aed17e94271a5b53725cc58859066c044cbc95bc:
normal entry calls installed update --adopt-installed before success, prepared
updater handoffs skip nested maintenance. Existing stopped/current state preserved;
maintenance, pinned Windows process-exit confirmation, machine-owner spawning,
readiness and SHA verification reused. Tracing existing verify_daemon_build found
retry spawned without stop; peer patch already adds stop_daemon before retry.
No overlapping edit/build/install. No blocking finding in this slice; posted
PR1470 comment5937900534 and canonical SOS. Expanded handoff fixture mocks daemon,
so does not prove actual replacement. Adoption case remains OPEN pending SAME
public installer rerun. Ordinary reverse AIRC ACK and Continuum command/result
remain separately OPEN; do not reuse transport aggregate ACK as acceptance.

2026-10-01T18:35Z independent exact aed17e9 handoff regression execution:
Compiler inventory empty and deployment claim absent before standalone rustc
fixture compilation; no workspace build or installer overlap. Isolated exact
install.sh, update_artifact.rs, update_shutdown.rs and update-handoff.rs fetched
by immutable commit. Windows fixture exits0, two tests PASS; delayed child exits
normally and pinned-handle wait distinguishes stop acknowledgement from process
exit. Installer fixture mocks daemon/package/auth work, proves adoption invocation,
failure propagation and suppression inside prepared updater handoff.
Mutation: SAME new fixture compiled with old b172f6e install.sh fails exit101 at
line191: actual build only vs required build+adopt. Original source bytes restored
in finally. Logs: state/team-proof-20260921/review-handoff-aed17e9/result.log and
old-installer-result.log. Exact evidence sent canonical SOS. No host install,
consent, restart or auth mutation. Live BIGGIEDESK adoption remains OPEN. PR1470
still aed17e9, CI remaining macOS/Windows Rust and PS5 at initial refresh; no merge.
Full100 development inbox JSON filtered before output; no return ACK found.

2026-10-01T18:45Z second live public run failed; peer owns recovery:
BIGGIEDESK reports aed17e9 normal installer completed firewall/startup after Joel
approved gsudo, then stop was acknowledged but pinned-process wait timed out.
Canonical IPC is down; remaining process serves a separate legacy socket. Shared
informational daemon.pid selected wrong endpoint owner. No manual kill/restart;
owner explicitly requested no competing intervention and retains next public run.
Independent review of exact98502ba81cf719bc30061a6d964eec350add4a53 completed:
GetNamedPipeServerProcessId on actual client connection selects serving PID;
wait handle pinned before stop, no production PID-file ownership inference.
Separate runtime thread avoids nested Tokio block_on; existing transport's bounded
connect retry reused. Explicit install now starts/verifies missing daemon, enabling
rerun after interrupted handoff; ordinary update stopped policy preserved.
New tests inspect distinct child endpoint despite misleading shared PID file,
plus missing/current/interrupted adoption. SOURCE REVIEW ONLY on this revision,
no new build or fixture execution. APPROVE slice subjectCI/live acceptance posted
PR1470 comment5938232383 and SOS. Prior aed17e9 passing fixtures are historical.
Case remains OPEN; leave legacy endpoint untouched. Full100 development inbox
JSON filtered before output, only old BIGGIEDESK probe, no reverse ACK. Upstream
13485 OPEN4 unchanged2026-09-28T18:20:15Z; last reviewed comment5875946656 retained.

2026-10-01T18:55Z peer reports third public install98502ba built/installed,
then fresh gsudo invocation returned999/Windows operation canceled before daemon
adoption. Earlier approvals do not authorize this canceled request. Owner already
asked Joel whether prompt appeared; no duplicate request/intervention here.
Independent bounded refusal check: existing PS5 bridge fixture with simulated
native firewall exit999 replacing73 passes and checks thrown exit999. Original
fixture bytes restored finally. Latest98502ba and b172 native install.ps1 share
blob4df5d2e13d9ae0b6dfed709ddbf637eab0a5b8a0, so tested public entry is current.
Evidence review-bridge-b172f6e/cancel-999-result.log. No real package/auth/UAC;
this verifies existing failure propagation for public FirewallOnly, NOT actual
prompt appearance, normal full Bash exit path, consent or successful adoption.
No new code defect established by this check; existing nonzero-refusal behavior
handles999. Reported exact limit to owner. Peer remains sole installer owner.
Full100 inbox JSON privatefiltered: original peer probe only, no returnACK.
CI exact98502ba all listed completed jobs pass, WindowsRust stillrunning initially;
no merge or local install. Remote onboarding case remains OPEN.

2026-10-01T19:05Z bounded SOS lifecycle repair, separate from peer installer:
Local source review found start_sos_fallback returned raw JoinHandle while claiming
Drop cancels polling. Tokio drops detach that task; an exited/canceled join in a
surviving runtime can retain a poller consuming its SOS cursor. Replaced with a
scoped owner aborting its own task on Drop. Regression starts two real delayed
pollers, drops each independently and verifies cancellation without network I/O.
Focused CLI test PASS1, build84s; fmt and workspace/all-target Clippy PASS.
Independent reviewer /root/review_sos_collision APPROVE, requested cooperative
cancellation limit in comment; included. Already-running synchronous gh call may
finish before next await; no subprocess termination claim. Not installed yet.
Prior receipts committed57ce2ab on preserved docs branch, then carried into new
codex/sos-fallback-lifetime based current canary9fdb8c1; no peer branch edits.

During this run peer reported fourth public installation98502ba EXIT0 after
actual Joel consent; CLI=daemon98502ba independently checked by that peer, canonical
PID7800/windowhandle0. This closes peer-reported normal adoption on that run, not
all onboarding. Legacy18484 on different daemon-v5 socket remains owner-investigated;
visible terminal source unproved. No manual kill. Received real ordinary event
e5edb496-a5ad-43e9-971b-48faf8fc0318 from BIGGIEDESK with fresh challenge. Replied
AIRC-only bf805d03-a8f6-40d6-9cc9-298c3aec28a2 with echo and fresh return token.
No token copied to SOS. Reverse echo pending; Continuum remote command unproved.
