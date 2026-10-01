# Windows onboarding beta: repeatable setup, not machine repair

## Acceptance contract

The public bootstrap must acquire prerequisites, plan storage, show GitHub's
device code/browser consent, wait for the user, verify the account, install,
join through the supported lifecycle, and report health. A rerun must reuse
completed steps and consent. Windows and POSIX use the same orchestration;
Windows adapters handle package application, volume/path translation, registry
PATH and process boundaries. `install.sh` remains the source-build coordinator.

GitHub approval belongs to the signed-in user's normal account. An agent must
not replace this product stage with repeated manual `gh auth login` commands.
The `setup/github-auth.sh` stage is shared by the public entry points. Its
output is deliberately not captured so the device code remains visible.

## Observations and manual contamination

This machine was not pristine by the time the repair branch was tested:

| Observation / intervention | Product requirement |
| --- | --- |
| Git bundled with the agent lacked usable HTTPS transport; full Git was installed separately | Detect usable Git for Windows/Bash before source acquisition |
| GitHub CLI MSI waited behind consent the user could not see; agent stopped its own installer | Use official checksum-verified per-user CLI distribution; reserve elevation for actual machine requirements |
| Repeated agent-started device logins expired/interrupted before the user could finish | Show code, await completion without an agent timeout, resume in setup |
| Official portable gh installed manually | Encode and test the same acquisition in the Windows adapter |
| Rust homes manually moved to a secondary fixed volume through user environment variables | Plan capacity before prerequisites and preserve configured homes |
| Source `target` manually junctioned to secondary storage | Honor Cargo's effective target directory; never require a junction |
| Process TEMP/TMP manually redirected | Adapter supplies temporary environment to the common coordinator |
| VS install returned 2147942512 / 0x80070070 | Report insufficient space accurately, not generic “modify Visual Studio” advice |
| Native installer duplicated build, identity and integration logic | Thin Windows entry dispatches to existing shared install.sh |
| Bootstrap ran daemon health checks before creating a daemon | Join with feed attachment disabled, then verify health |
| Native installer printed manual gh login as a next step | Public setup owns authorization and reuse |

No access tokens or device codes belong in these notes. Existing authenticated
account was verified via GitHub API. SOS coordination uses the existing
account SOS gist; no duplicate rendezvous record was created.

## Mandatory intervention-to-installer checklist

**No row is closed merely because this machine works.** Implementation and a
reproduction of the original failing state are separate requirements. Every
OPEN row blocks claiming that fresh onboarding is fixed.

| Intervention / failure | Checked-in replacement | Acceptance status |
| --- | --- | --- |
| Full Git manually installed because agent-bundled Git could not use HTTPS | `install.ps1` checks HTTPS helper and acquires Git/Bash | **PARTIAL:** public-entry missing-HTTPS fixture passed; actual first Git package acquisition still unproven |
| gh installed manually from official ZIP after invisible MSI consent | `windows/install-prereqs.ps1` per-user release ZIP, checksum verification, PATH registration | **PARTIAL:** missing-gh, checksum, executable, reuse and corruption fixtures passed; real run reused gh, so live acquisition still unproven |
| Repeated manually launched GitHub login | `setup/github-auth.sh`, invoked by public installers and bootstraps | **OPEN:** isolated approval/cancellation/reuse fixtures passed; real public rerun reused approval; first live public consent still unproven |
| Rust homes hand-set on D: | `windows/install-prereqs.ps1` storage planning and persisted RUSTUP_HOME/CARGO_HOME | **OPEN:** real rerun preserved prior manual homes; fresh selection/install needs reproduction |
| Manual target junction | Adapter uses Cargo metadata from checkout and supplies CARGO_TARGET_DIR when unconfigured | **OPEN:** rerun uses independent selected D: target; configured-cache and no-junction regressions required |
| Manual TEMP/TMP overrides | Adapter returns explicit process environment to `install.sh` | **OPEN:** test inherited temp selection across vendor elevation |
| VS default install failed with 0x80070070 | Adapter plans secondary volume and supplies install/cache/shared paths; preserves actual failure code | **PARTIAL:** same public bootstrap installed missing VS successfully on D:; vendor fixed-location C: costs remain |
| Registry PATH refresh selected System32 WSL bash.exe | `install.sh` preserves current Bash runtime before imported Windows PATH | **VERIFIED:** real public rerun passed this failure; full coordinator fixture passed and negative control reproduced the old failure |
| Stale agent process could not see installed tools/Rust homes | Bridge refreshes PATH; adapter reads persisted Rust homes | **OPEN:** process-boundary fixture needed; manual environment refresh used only for local fmt check, not installer acceptance |
| Older source bypasses updated coordinator | Native bridge and shared installer acquire a compatible sibling checkout for older installer-owned source and preserve explicit developer trees | **PARTIAL:** native-entry and shared downloaded-entry fixtures pass; live downloaded-entry acceptance remains open |
| Public canary entry cloned main without helpers | Fresh source defaults to canary; bootstrap acquires missing helper stages | **PARTIAL:** `test/setup-source-acquisition.sh` exercises the downloaded entry with older managed source; live published entry remains unproven |
| Existing airc executable caused bootstrap to skip repaired setup | Both public bootstraps unconditionally reconcile through the installer | **PARTIAL:** third live public run reused prerequisites and reached the repaired firewall stage; successful end-to-end repeat remains open |
| Health checked before join; join feed never returned | Both bootstraps use AIRC_NO_ATTACH for provisioning then doctor health | **OPEN:** final installed-binary join/health and peer acknowledgement pending |
| Firewall verification failure was suppressed and install continued | `windows/configure-firewall.ps1` captures elevated exit and verifies state; coordinator now fails on failure | **PARTIAL:** real repeated bootstrap correctly returned exit 1 on Windows cancellation; actual approval + verified rule remains unresolved |
| Firewall elevation lost argument boundaries for paths with spaces | File-based firewall adapter with quoted native argv | **PARTIAL:** `test/windows-firewall-process.ps1` passes actual child-process argument boundaries, including spaces and apostrophes; live consent remains open |
| Fresh-space reserve rejected an already-installed rerun | Windows adapter uses Cargo's effective target and remaining-work reserves, including system-only installs without a relocation marker | **PARTIAL:** secondary-volume, system-volume first-install/rerun, configured-cache and exhausted-cache fixtures pass; live final rerun remains required |
| Existing daemon may retain an old PATH after prerequisites change | Installation must reconcile daemon lifecycle using the supported startup/join path | **OPEN investigation:** portable gh is absent from runtime fallback paths; inspect daemon registry diagnostics and process age before attributing missing enrollment |
| Health command passed while no same-account peers were enrolled | Record explicit account-registry enrollment and two-way peer acknowledgement as mesh acceptance evidence | **OPEN:** zero enrolled peers observed; doctor exit 0 is not proof of the requested connection |
| Windows startup registration failure can remain a warning | Review native startup failure propagation in the shared coordinator | **OPEN audit:** verify promised startup behavior and ensure failure cannot be reported as successful installation |
| Local Rust 1.99 Clippy gate fails in dependency-generated code | Integrate and validate the narrow upstream compiler-compatibility fixes | **VERIFIED locally:** applied Bigmama PR #1469's async-trait 0.1.92 lockfile and doctor closure changes (24b88d0 / 34829dc); fmt and clippy --all-targets -- -D warnings pass on Rust 1.99; CI still required |
| Initial wrong branch | Repair branch is based on origin/canary | Corrected; draft PR #1470 tracks the changes and unresolved acceptance work |
| Shared installer guessed source/target after failed Cargo metadata | Shared target resolver preserves command failure and rejects missing/empty/malformed output | **PARTIAL:** public artifact preparation fixture exercises exit 42 and malformed/missing/empty target with a valid stale default binary present; configured target must win |
| Firewall verifier rejected Enforced plus ProfileInactive | Accept effective enforcement on an active profile with inactive alternative profiles; still reject policy/address failures | **PARTIAL:** reproduced by actual Windows CI provider; regression passes; corrected real-provider and installer CI rerun required |
| AIRC requests elevation separately from Continuum's existing elevation session | Integrate with manifest-driven machine-scope stages and existing Ensure-Elevated / Invoke-Elevated / Clear-Elevation lifecycle | **OPEN:** located Continuum install-manifest.toml, generated projection, and install-common.ps1; do not add manual gsudo installation or another unrelated privilege mechanism |
| Continuum skips AIRC reconciliation when an executable exists and maintains its own firewall policy | Have the consumer invoke the supported AIRC reconciliation and shared firewall acceptance through its owned elevation session | **OPEN:** Mod-Airc and Mod-AircFirewall inspected; existing manifest source points at main; source/channel compatibility and generated projections need coordinated repair |
| Agent suggested manually selecting firewall profile checkboxes | Installer owns application-specific TCP and UDP local-subnet rules and effective-policy verification | **PARTIAL:** policy/process regressions pass; public bootstrap rerun reached its own Windows consent stage; live result pending |
| Firewall helper allowed only TCP although LAN presence uses UDP | Shared installer invokes the Windows helper before startup; helper reconciles both protocols | **PARTIAL:** missing/legacy/blocked rules, Public profile, idempotence and unrelated-rule preservation fixtures pass; mesh proof pending |
| Unelevated firewall reads fail with Access Denied on this machine | Helper distinguishes read restriction with exit 4; elevated application verifies ActiveStore policy before success | **PARTIAL:** restricted-read process fixture passes; live elevated result pending |
| Enabled firewall rules can still be unenforced | Verification requires Full EnforcementStatus as well as matching application, protocol and local-subnet filters | **PARTIAL:** managed-policy and address-resolution rejection fixtures pass; no organization settings changed |

User clarified the visible dialog was Defender Firewall's application prompt,
with Public selected and Private unselected, not a confirmed UAC dialog.
Read-only inspection found the active connection classified Public. The agent's
Private-only checkbox advice was incorrect and was not performed. Setup must
handle the policy itself, without changing the connection category. Any Windows
administrator consent is requested by setup, not replaced with manual rule edits.

The acceptance scope includes Continuum: AIRC supplies its local and P2P grid.
Same-account gist rendezvous and automatic LAN connection must work through the
supported setup before claiming readiness for grid ML commands. Future friend
invitations and QR exchange are product context, not changes implemented here.

AIRC is also the agent/person communication bus and Continuum's transport for
remote command handles, events and streams, including inference, training,
rendering and WebRTC. A mesh hello is the first check; end-to-end acceptance
also needs a supported Continuum remote command and returned result/events.
AIRC also carries workspace state, Kanban cards and team orchestration. Git is
the usual versioned source; LFS, container registries, Hugging Face and other
versioned stores serve larger artifacts. These are integration context, not a
request to redesign storage during install recovery.

The user approved Windows PowerShell prompts during live attempts. The elevated
helper created both TCP and UDP local-subnet rules, then failed verification
because the associated ActiveStore objects reported NotApplicable enforcement.
Do not label that a user cancellation or claim the firewall is verified. Direct
rule-by-name hydration is implemented and fixture-tested, but not live-proven.
No installer is currently running. Consolidated elevation integration must be
resolved before creating another series of separate consent prompts.

Diagnostic scripts are permitted for investigation. Their effects never satisfy
these rows until implemented in the supported installation and verified there.

### Required closure evidence for every intervention

Before taking another diagnostic workaround, add its original failure and any
local state it changes to this table. A closure must name the checked-in setup
stage, a regression that starts with the original missing or failing condition,
and the result of rerunning the public entry point. Fixtures, a repaired local
machine, and a pristine installation are distinct evidence and must remain
labelled separately. Preserve nonzero failures rather than silently proceeding.

### Third public bootstrap attempt

The third invocation reconciled prerequisites despite the existing executable,
rebuilt through the shared installer, and reached firewall authorization.
Windows returned "The operation was canceled by the user." The installer
propagated exit 1 instead of continuing. This does not establish whether the
user saw the prompt, declined it, or encountered another consent problem;
visibility clarification is pending before another consent attempt.

Read-only inspection found the Codex shell on the active Windows session's
Default desktop. No Codex permission setting was changed, and no application
admin requirement has been established. No firewall approval, completed public
install, or peer connection is claimed from this attempt.

## Validation record

- Shared auth fixture: fresh consent, approval delayed beyond six seconds,
  successful rerun without reauthorization, missing gist scope, account mismatch,
  and cancelled approval. Passed locally under Git Bash.
- Actual shared auth stage invoked through Windows PowerShell 5.1 adapter:
  reused completed GitHub approval without another prompt.
- PowerShell parsing and Bash syntax checked locally.
- Real public `bootstrap-airc.ps1 -GitHubUser <account>` run started with the
  local canary-based repair branch. It selected a secondary fixed drive for
  build storage, preserved existing Rust homes, and invoked Visual Studio with
  explicit install/cache/shared paths. Full installation/mesh result pending.
- A 2 GB Windows system-volume floor and 20 GB build-volume reserve are
  preliminary preflight budgets, not proof of vendor package requirements.
  Visual Studio retains fixed-location system components and may preserve an
  existing shared-component location. Vendor diagnostics remain authoritative.

This rerun cannot establish a pristine install because of the interventions
above. Clean-state fixtures and Windows CI are required in addition to live
installation and idempotent rerun evidence. Stock hosted runners already have
many prerequisites; they alone do not prove first-time package acquisition.

## Outstanding acceptance work

- Complete the real bootstrap and verify same-account peer discovery.
- Rerun the same public command and show that consent, build, startup and join
  remain idempotent; no duplicate daemon.
- Exercise new prerequisite acquisition against a genuinely fresh Windows
  environment, including low system-volume space and invisible/declined UAC.
- Verify Linux/macOS regression jobs and obtain independent adversarial review.
- Integrate Continuum's consumer ordering separately; do not assume moving its
  cold-storage module alone relocates prerequisite downloads or fixed SDK files.

### 2026-10-01 16:38 UTC — hosted installer gates green

PR #1470 head d547fd4a passed run 36892134133: public clean-install jobs on
Windows, Windows PowerShell 5.1, Linux and macOS; setup-consent fixtures on all
three platforms; and the real elevated Windows firewall provider regression.
The shell guard run also passed. Rust-only CI was path-filtered; local fmt and
clippy checks were run before push. This verifies the mixed enforcement-status
repair on the hosted Windows provider, not this machine's completed setup.

Bigmama independently reproduced the firewall policy fixture on PS5.1 and
confirmed the metadata-failure regression is resolved. Review still identifies
Continuum's separate broad firewall rule and borrowed-cache ownership as open
integration work. Continuum branch codex/shared-windows-install-elevation now
contains the in-progress extraction of its existing elevation helper; AIRC does
not consume that artifact yet. No new live install or machine repair was run.
Standalone acquisition, one owned elevation session, public-entry local retest,
idempotent rerun and same-account peer discovery remain OPEN.

### 2026-10-01 16:57 UTC — shared elevation implementation ready for consumers

Continuum draft PR #4649 head 78cf0366b now exports a validated process-owner
context, supports child-first elevation and outer-only cleanup, invokes native
gsudo.exe, and preserves pre-existing caller caches. Review found and resolved
a no-work cleanup regression that could close a caller's existing cache. The
existing Windows service/installer suite passes, including real PowerShell and
Git Bash child ancestry, borrowed cleanup, stale context and external-cache expiry.
Independent review approves the helper scope subject to CI, which is pending.

AIRC does not consume this helper yet. Pinned/integrity-checked standalone
acquisition, all AIRC privileged stages, its exact shell adapter, and Continuum's
canonical-firewall delegation remain OPEN. No new local installer, consent,
firewall write or peer-connection verification was performed. AIRC head 627bf3a
also incorporates Bigmama's foreground/background SOS cursor repair; its Windows,
Linux and macOS clean-install CI passed, with the PS5 job still pending at this
checkpoint. The documentation and helper fixtures do not close live acceptance.

### 2026-10-01 17:08 UTC — startup failure and peer-probe checkpoint

The shared installer's Windows autostart stage still converted registrar failures
into warnings. It now stops setup. The existing autostart fixture executes that
exact Bash stage with success/failure results in native and existing-only modes,
and passes through the PS7/Git Bash/PS5 boundary. Independent review approves this
scope subject to CI. Public-entry live retest remains OPEN; no task repair or UAC
was performed by the regression test.

Bigmama reports seeing machine peer 0121d959 in same-account discovery. A normal
room publish from this repo produced event 08c65378-155d-4873-b1c4-c054a6067936,
reporting an answering daemon and three LAN links. Its author d4ad790b is this
repo's agent identity, while status reports the machine-account daemon 0121d959.
An initial concern about that difference was corrected after inspecting scope
resolution and reading the explicit machine home; both belong to joelteply.
No manual enrollment, dial, identity change or restart was done. The nonce was
sent only through AIRC; an ordinary peer echo is still required before claiming
delivery. Neither discovery nor a publish receipt closes mesh acceptance.

### 2026-10-01 17:29 UTC — standalone consumer of the shared elevation owner

AIRC now acquires the generated Windows dependency manifest and small elevation
helper from immutable Continuum commit 0aad524a004cc1a95e1509a3170449cff2e76a89.
Both artifacts must match repository-pinned SHA256 values before either executes;
verified cache reuse, corrupt-cache repair and download mismatch rejection have
regression coverage. This does not install the Continuum application. The gsudo
package descriptor comes from the existing canonical manifest.

Native setup owns the session through completion. Direct Git Bash setup re-enters
the same coordinator under a native process owner; Continuum callers retain their
outer owner. MSVC, firewall and fallback startup registration use that shared
helper, with original child failures preserved and cleanup in finally blocks.
Git and Rust acquisition explicitly select ordinary-user package scope.

Independent review found an upgrade defect: the previous source layout contains
the auth helper but lacks the new session adapter. Both entry points now validate
the required Windows layout and select another managed sibling when a cached
fallback is obsolete, preserving older trees and their local work. Native and
Bash regression fixtures exercise both preceding-release and stale-fallback cases.
The bridge, firewall process, storage and autostart suites pass. The autostart
suite also runs the real pinned helper through AIRC's exact PS7 / Git Bash / PS5
adapter and verifies borrowed ownership without package installation or elevation.
One initial diagnostic PS5 test launch hit the shell execution policy; rerunning
the fixture with process-only ExecutionPolicy Bypass passed, with no policy change.

The adversarial reviewer approves this scope subject to CI. Prior head ad9e408
passed all four hosted clean-install platforms. This new consumer still needs
its own hosted run, fresh gsudo acquisition and the normal local public-entry
consent/idempotency test. Continuum's canonical-firewall delegation and two-way
ordinary peer delivery remain OPEN. No live installer, UAC, firewall mutation,
task repair or manual peer enrollment was performed for this checkpoint.

### 2026-10-01 17:40 UTC — full Windows owner chain regression

Hosted 944440f Windows/PS5 clean installs stopped at shared-owner validation:
"Cannot resolve installer ancestry." The shortened PS7/adapter/PS5 fixture had
passed but omitted the coordinator's additional Bash-to-Bash process launch.
The full existing windows-setup-path fixture now imports the real pinned helper
inside its prerequisite child and requires borrowed ownership. This reproduced
the CI failure locally. Keeping only the launcher alive was insufficient; MSYS
could still leave a native parent ID pointing at an exited intermediate Bash.

The coordinator now sources its PowerShell launcher inside a waiting subshell,
and the launcher invokes PowerShell without exec/env process replacement. This
preserves environment isolation, child status and the live ancestry chain without
weakening owner validation. The full fixture passes with real owner/reentry and
borrowed context; package acquisition and real cache operations remain disabled
in the fixture. Its older synthetic source also now contains the required layout.
An autostart fixture launch initially used a stale shell's unconfigured Rust home;
the diagnostic was rerun with the previously established D-drive toolchain env.
No Rust default or local installer state was manually changed. CI and live public
entry acceptance remain OPEN; firewall delegation waits behind this failure fix.

### 2026-10-01 17:47 UTC — one firewall owner across both public installers

The native public entry now accepts `-FirewallOnly -AircPath` for an existing
executable, uses its usual compatible-source acquisition and elevation lifecycle,
and delegates to the same canonical firewall adapter. It does not enter the build,
auth or startup lifecycle. The existing bridge fixture verifies literal paths,
absence of a build handoff and propagation of policy failure. Continuum PR #4649
uses this public mode through its existing AIRC manifest source, now aligned with
canary; its separate broad firewall-rule creator has been removed. Its service
suite passes with coverage for manifest URL, inherited owner, paths and failure.
Merge ordering requires this AIRC entry to land before the Continuum consumer.
No local firewall rules were changed; live public-entry acceptance remains OPEN.

Adversarial review found a migration defect before publication: both canonical
rules could coexist with Continuum's old Any-remote allowance and pass check-only.
Canonical AIRC verification now rejects enabled exact-program inbound allowances
outside LocalSubnet, triggering its existing scoped reconciliation. The policy
fixture covers coexistence and convergence; unrelated program/outbound policy
remains preserved. An unchangeable managed allowance still prevents a false
success receipt. This was a repository/fixture repair, not a local rule change.

### 2026-10-01 17:58 UTC — native status shadowing in Windows entry

CI exposed a bridge regression after the deliberate firewall failure test. A
stronger local reproduction showed that a caller-scoped LASTEXITCODE can shadow
the global automatic value written by native processes. The native public entry
now reads the global native result explicitly at Git, package, source, firewall
and coordinator boundaries. The fixture uses a native Git executable instead of
a PowerShell script pretending to update native status; it seeds a stale failure
for successful setup and stale success for an actual failing firewall child.
Both cases pass while preserving developer-source refusal. This is a product
fix as well as a fixture correction, not a bypass of the CI failure.

Bigmama reports normal public update to 9fdb8c1 completed locally with matching
CLI/daemon revisions and the corrected project peer projection. That is Bigmama's
receipt, not this machine's public-entry or reverse-event acceptance. Joel must
be present for any required local consent; no unattended approval is assumed.

### 2026-10-01 18:07 UTC — confirmed CI PATH overflow

The exit-code repair did not resolve the hosted developer-source fixture failure.
Exact diagnostics in bcfea09 revealed "Environment variable name or value is too
long." Native Refresh-Path appended registry paths on every entry/finally, so
repeated installs exceeded the Windows limit on CI's longer PATH. A long inherited
PATH reproduced the failure locally. Refresh now deduplicates case-insensitively
while preserving User, Machine and session precedence. The existing bridge suite
seeds 24 KB of repeated entries and passes all cases. This preserves session-only
paths and does not modify registry PATH. Independent review approves the repair;
hosted rerun remains required. No live installer has started while Joel's presence
for protected Windows consent remains unconfirmed.

### 2026-10-01 — first completed BIGGIEDESK public-entry run

With Joel available, stock Windows PowerShell ran the repository's normal
`install.ps1` with no prerequisite/build/auth skip flags or diagnostic toolchain
overrides. It automatically selected D-drive build storage, reused existing
Git/Rust/MSVC and joelteply authorization, built b172f6e, and acquired gsudo 2.6.1
portable for this user through the shared manifest. The administrator step then
verified effective TCP/UDP LocalSubnet rules; startup registration and agent
integrations completed, and the installer exited 0. Toolchains/auth were reused,
not a fresh-install proof; gsudo acquisition and firewall application were live.

Post-install verification found CLI b172f6e but daemon a3e04c6. No manual restart
was performed. Normal setup now invokes `update --adopt-installed`, reusing the
updater's maintenance lock, pinned-process shutdown wait, detached restart and
build verification. Stopped/current daemons remain untouched. Prepare/prebuilt
handoffs skip this inner step because their updater already owns maintenance.
The retry verifier now stops before spawning a replacement. The existing handoff
fixture covers normal adoption, failure preventing success, and nested-handoff
skip. This case stays OPEN until the same public entry proves daemon adoption.

### Second public run — endpoint ownership and interrupted adoption (OPEN)

The ordinary entry built and installed aed17e9, reused prerequisites/auth, and
completed firewall/startup after Joel approved gsudo. Adoption failed honestly:
the stop response arrived, but the process-handle wait timed out. Canonical IPC
was then unavailable; the remaining process used a different legacy socket.
The updater had trusted the shared, informational machine-home daemon.pid file,
which is not endpoint ownership evidence when legacy and canonical sockets
coexist. No process was manually killed or restarted.

The Windows transport now obtains the serving process ID from the actual named
pipe connection and pins that process's wait handle before requesting shutdown.
Explicit installation also starts/verifies a missing daemon, so rerunning the
public entry recovers an interrupted handoff. Ordinary update retains its
previously-stopped policy. Regression coverage exercises distinct process
ownership and missing/current/retry adoption. These remain OPEN pending the
next full public run and peer event evidence.
