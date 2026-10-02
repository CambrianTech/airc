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
| Existing airc executable caused bootstrap to skip repaired setup | Both public bootstraps unconditionally reconcile through the installer | **VERIFIED:** ordinary install.ps1 rerun on 98502ba reconciled existing installation, exited 0 and verified the running build |
| Health checked before join; join feed never returned | Both bootstraps use AIRC_NO_ATTACH for provisioning then doctor health | **OPEN:** final installed-binary join/health and peer acknowledgement pending |
| Firewall verification failure was suppressed and install continued | `windows/configure-firewall.ps1` captures elevated exit and verifies state; coordinator now fails on failure | **VERIFIED:** cancellation returned exit 1; approved public rerun verified effective TCP/UDP rules and completed |
| Firewall elevation lost argument boundaries for paths with spaces | File-based firewall adapter with quoted native argv | **VERIFIED:** process-boundary fixture passed; approved normal entry completed effective-policy verification |
| Fresh-space reserve rejected an already-installed rerun | Windows adapter uses Cargo's effective target and remaining-work reserves, including system-only installs without a relocation marker | **VERIFIED for rerun:** fixtures pass; normal entry automatically selected D: and completed with under 2 GB available on C:; fresh toolchain acquisition remains separately open |
| Existing daemon may retain an old PATH after prerequisites change | Installation must reconcile daemon lifecycle using the supported startup/join path | **OPEN investigation:** portable gh is absent from runtime fallback paths; inspect daemon registry diagnostics and process age before attributing missing enrollment |
| Health command passed while no same-account peers were enrolled | Record explicit account-registry enrollment and two-way peer acknowledgement as mesh acceptance evidence | **VERIFIED:** 19 enrolled records include same-account LAN peers; public AIRC challenge and independent Bigmama return event observed after normal installation |
| Windows startup registration failure can remain a warning | Review native startup failure propagation in the shared coordinator | **OPEN audit:** verify promised startup behavior and ensure failure cannot be reported as successful installation |
| Local Rust 1.99 Clippy gate fails in dependency-generated code | Integrate and validate the narrow upstream compiler-compatibility fixes | **VERIFIED locally:** applied Bigmama PR #1469's async-trait 0.1.92 lockfile and doctor closure changes (24b88d0 / 34829dc); fmt and clippy --all-targets -- -D warnings pass on Rust 1.99; CI still required |
| Initial wrong branch | Repair branch is based on origin/canary | Corrected; draft PR #1470 tracks the changes and unresolved acceptance work |
| Shared installer guessed source/target after failed Cargo metadata | Shared target resolver preserves command failure and rejects missing/empty/malformed output | **PARTIAL:** public artifact preparation fixture exercises exit 42 and malformed/missing/empty target with a valid stale default binary present; configured target must win |
| Firewall verifier rejected Enforced plus ProfileInactive | Accept effective enforcement on an active profile with inactive alternative profiles; still reject policy/address failures | **PARTIAL:** reproduced by actual Windows CI provider; regression passes; corrected real-provider and installer CI rerun required |
| AIRC requests elevation separately from Continuum's existing elevation session | Integrate with manifest-driven machine-scope stages and existing Ensure-Elevated / Invoke-Elevated / Clear-Elevation lifecycle | **OPEN:** located Continuum install-manifest.toml, generated projection, and install-common.ps1; do not add manual gsudo installation or another unrelated privilege mechanism |
| Continuum skips AIRC reconciliation when an executable exists and maintains its own firewall policy | Have the consumer invoke the supported AIRC reconciliation and shared firewall acceptance through its owned elevation session | **OPEN:** Mod-Airc and Mod-AircFirewall inspected; existing manifest source points at main; source/channel compatibility and generated projections need coordinated repair |
| Agent suggested manually selecting firewall profile checkboxes | Installer owns application-specific TCP and UDP local-subnet rules and effective-policy verification | **VERIFIED:** approved public installer configured and verified TCP/UDP LocalSubnet policy on the existing network profile |
| Firewall helper allowed only TCP although LAN presence uses UDP | Shared installer invokes the Windows helper before startup; helper reconciles both protocols | **VERIFIED:** fixtures pass; real public install verified both protocols and subsequent two-way Bigmama event exchange succeeded |
| Unelevated firewall reads fail with Access Denied on this machine | Helper distinguishes read restriction with exit 4; elevated application verifies ActiveStore policy before success | **VERIFIED:** real unelevated read returned restricted access; the same installer administrator child verified effective policy before success |
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

### 2026-10-01 19:07 UTC — public installation and two-way bus verified

Joel identified the prior gsudo cancellation as a timeout. A following run was
interrupted before approval at his request, then the ordinary install.ps1 entry
was rerun after he said he was ready. It built/installed 98502ba and completed
firewall, startup and daemon adoption with exit 0. Independent status reported
CLI and canonical daemon 98502ba81cf7. The newly spawned daemon PID 7800 had no
window handle; Joel confirmed the previously visible airc.exe window had closed.
Startup uses the installed windowless wscript launcher. No manual daemon repair
or prerequisite/build skip was used; existing authorization/toolchains were reused.

The ordinary cambriantech send produced event e5edb496-a5ad-43e9-971b-48faf8fc0318.
Bigmama returned event bf805d03-a8f6-40d6-9cc9-298c3aec28a2, echoing the AIRC-only
challenge and supplying a fresh return token. This machine read that event through
the canonical daemon's public inbox and echoed the return token through public
msg. No challenge token was conveyed over the SOS gist. This is two-way event
evidence following public installation, beyond registry/address-book discovery.

Remaining OPEN: the legacy daemon PID 18484 still serves the same machine identity
on the historical parent-directory daemon-v5.sock endpoint using a3e04c6. It was
not killed manually. The installer must retire verified same-account legacy
endpoints while preserving foreign/isolated scopes and the healthy canonical
daemon. Clean hosted tests passed for Windows (including PowerShell 5), Linux and
macOS on 98502ba; that does not erase the live legacy-migration defect.

The migration repair normalizes machine-account path comparisons before socket
selection. Public adoption inspects only the two historical account endpoints,
requires the verified canonical identity and current IPC protocol, and stops each
matching endpoint through its own pinned process handle. Foreign identities are
left untouched; uncertain inspection errors stop setup. Canonical build is checked
again afterward. Real IPC responders cover same-owner, foreign and missing
endpoints; path-alias and isolated-scope regressions cover candidate selection.
Live migration acceptance remains OPEN until the normal installer exercises it.

### 2026-10-01 19:23 UTC — migration and identical rerun verified

Normal install.ps1 on 077158a completed with consent and explicitly retired the
verified legacy parent-directory endpoint, then verified build 077158a9a2ed.
Independent process inventory found only canonical daemon PID 1628, with no
window; CLI and daemon revisions matched. An immediate identical public-entry
rerun also exited 0, reported the installed build already running, and preserved
PID 1628 and its uptime. No skip flags, manual stop, kill or restart were used.
This closes the live legacy-migration and idempotent-adoption cases. A fresh
post-migration challenge was sent over AIRC. Bigmama returned event
793e793b-4c84-4a00-8c4e-5c5ce0b57cb1 with the exact challenge and a fresh token;
this machine read it through the canonical daemon and echoed the token over
AIRC. No token was shared through the SOS gist. Two-way messaging therefore
also passed after legacy migration and the identical rerun.

Continuum remote-command acceptance stays OPEN: Bigmama's public read-only ping
to this node timed out (correlation f0ca974e-6a36-4f4b-8c18-cf9685c36b22). This
account has no discoverable continuum executable after registry PATH refresh,
no ~/.continuum directory, and no running continuum/core process. Standalone
AIRC messaging is not a claim that a Continuum command receiver is installed.
### Hidden setup descendants and archive acquisition (repository repair)

AIRC now uses its checksum-pinned shared launcher for Git/winget acquisition,
prerequisite probes, firewall child checks, device authentication and Bash setup
coordinators. Coordinators own their descendants until exit zero and drained
output; only successful completion permits the intended daemon to survive.
Bootstrap embeds a generated projection of the same artifact loader, with CI
rejecting drift. Both source-layout checks reject older incomplete adapters.

PS5 scratch tests passed for native no-console Git/Bash/gh fixtures, paths with
spaces/apostrophes, source upgrade/preservation, immutable artifact cache and
checksum refusal, storage planning, firewall process/policy and consent refusal.
No real firewall, PATH registration or daemon changes occur in these fixtures.

A real PS5 Expand-Archive hang reproduced with a tiny local ZIP independently
of mocks. The GitHub CLI acquisition path now uses framework ZIP extraction into
its unique staging directory, validates the staged executable, then publishes and
validates the installed executable. Acquisition/reuse/corrupt-download tests pass.
Pinned-script downloads use bounded HttpClient body acquisition after PS5 web
response processing also stalled. These are repository changes, not local installs.

The current helper pin is a review-branch dependency (Continuum 71fb9cc51).
Release repinning, final CI/review and public live acceptance remain OPEN.

Released-helper validation: Continuum PR #4658 merged after all required CI and independent review at c40cc9cc6d3ba086d83ac25c330116def24b80ed. The standalone artifact lock now pins that released revision; generated bootstrap drift check and all five PS5 setup/firewall/storage/acquisition regression suites pass. Actual public installation and consent acceptance of this change remain open.


### 2026-10-02 — inherited PowerShell module recovery

A real Continuum public entry under Windows PowerShell 5 inherited PowerShell 7
module paths from its desktop host and failed while loading Security/Get-Acl.
Standalone AIRC also uses Get-FileHash before importing the shared launcher.
Its generated bootstrap now projects the runtime initializer from the same
checksum-pinned helper before artifact verification or FirewallOnly validation.
It imports the executing runtime's built-in modules without rewriting the
caller's PSModulePath. No Continuum application installation is required.

The actual fresh PS5 public-entry fixture passes with an incompatible Security
module first in PSModulePath; removing the generated initializer makes the same
fixture fail with the foreign-module sentinel. The child asserts inherited paths
are unchanged. The full setup bridge and the four storage/acquisition/firewall
suites pass. These isolated fixtures do not modify the live firewall or daemon.
The released helper revision depends on Continuum #4660; repinning to its
released merge, CI, and live installation acceptance remain OPEN.

Released dependency and CI fixture correction: Continuum #4660 merged all-green
at d2604d832da2b47f7ba3680761c5a46d6b23fc2b. The lock and generated entry now
pin that released revision and retain the verified artifact hashes. Initial
Windows CI showed that PS5 startup can reorder inherited module paths, defeating
the negative control. Both fixture children now establish the identical foreign
module path before invoking the actual public entry, then assert preservation
of that exact baseline. The unfixed control fails and the repaired entry passes;
full bridge regression passes locally. Independent review approved this delta.
Final-head CI and public live acceptance remain OPEN.

The second Windows CI control still did not select the foreign module; its
host-specific autoload cause is not established. The fixture now explicitly
loads the same foreign Security module in both fresh child processes before the
public entry. This proves recovery from an already selected incompatible module
without depending on host autoload ordering. The fixed entry preserves module
paths; removing its initializer fails. Independent review approved this delta.

Final fixture targets Utility/ConvertFrom-Json, the generated loader's first
module-provided command. CI diagnostics proved the Security control exited zero
even though the foreign hash command was initially selected; its intervening
module-loading behavior is not a reliable control. The first-command fixture
avoids that later boundary and tests autoload directly, with no pre-import of
the foreign module. The same initializer removal remains the negative control.

### 2026-10-02 — doctor account-trust snapshot (OPEN)

Live diagnostics reported zero OwnAccount peers while `airc peers` reported
three. The delivery check read only the scope trust store; the peers command
already uses the canonical merged scope/machine snapshot. The repair makes
doctor use that same reader, including deduplication and local-identity
exclusion. No live enrollment, trust tier, or daemon was changed.

The CLI regression uses isolated temporary trust records to check remote
OwnAccount inclusion, other-tier exclusion, and local-identity exclusion.
Existing library snapshot regressions cover machine-only enrollment and scope
precedence. The new CLI regression passed (1/1), existing canonical snapshot
tests passed (3/3), and `cargo clippy --all-targets -- -D warnings` passed.
Formatting and diff checks passed. Independent parent review approved the
implementation. Validation used the shared hidden launcher, two compiler jobs,
and D: build/temp paths; C: remained at 7.5 GiB free and RAM above 39 GiB free.
Live installed diagnostic reconciliation remains OPEN.


### Hidden PowerShell diagnostic propagation (2026-10-02)

OPEN until normal public installation exercises this integration. Continuum's
public Windows build failed with Cargo 101 while its actual compiler stderr was
lost at a hidden PS5 host boundary. Reproductions isolated unmerged ErrorRecord
rendering; native pipe draining itself worked. AIRC uses the same nested host
boundaries and now consumes the canonical entry serializer instead of inventing
another launcher or output policy.

The lock references released Continuum 10cd21d7781b55789cdc1563c1f96e1304a599fc
with verified artifact hashes and regenerated entries. The generator projects verified runtime
initialization and entry serialization definitions to setup-entrypoint.ps1 and
the public entry. Native setup adapters reuse those definitions, including their
loader failures; shared-setup.ps1 remains a dot-source library. Source acquisition
requires the generated file, so an older incomplete checkout cannot be selected.

Actual PS5 bridge PASS 60617 includes real install-session.ps1 -File execution
with the existing harmless native stand-in: exactly one stderr diagnostic,
stdout data unchanged, native exit 23 preserved. Public failure fixtures now
verify exit 1 and the original message on stderr. Foreign-module negative control,
source acquisition, cache integrity and developer-source refusal passed. Firewall
process/policy and storage fixtures PASS 65204. Bash source acquisition and exact
Windows setup-path tests PASS 29767. No installer, consent, credentials, firewall
or live daemon action was performed. Startup fixture's first run could not find
rustc in this stale shell; registered Rust homes/PATH were supplied only to the
subsequent test process, not written to the installation or global environment.
Final startup/autostart and GitHub acquisition PS5 fixtures PASS 19485 with
registered toolchain paths. Bootstrap generator check and whitespace check pass.
These synthetic receipts do not establish a repaired live AIRC transport or a
new installed/running binary revision; those remain separate acceptance work.

Released shared helper pin: Continuum #4664 merged all-green reviewed at 10cd21d7781b55789cdc1563c1f96e1304a599fc. Actual standalone PS5 bridge repeated at released pin (86377) PASS, including hidden native coordinator stderr/data/exit and public entry fixtures. Parent review APPROVE; full live installation remains OPEN.
### Existing startup task token migration (2026-10-02)

OPEN: Bigmama reported IPC Access denied (OS error 5) while an old AIRC daemon
remained present but uninspectable to its normal user. Its actual scheduled-task
principal is Interactive/Limited (peer receipt 6403107). This separate legacy
migration defect does not explain that incident or repair the running process.

Code inspection found a concrete setup defect: existing same-SID airc-join tasks
preserved Principal verbatim, and the no-op guard/verification compared only the
action. A matching legacy Highest or S4U task therefore survived normal install.
Fresh tasks already use Interactive/Limited. Registration now applies that same
runtime token contract to existing same-account tasks, verifies SID/logon/run
level as well as action, and includes principal-only changes in pending restart.
Triggers/settings remain intact; foreign-account or unverifiable ownership still
refuses. Elevated setup can repair task registration but must not elevate runtime.

Validation: existing startup fixture now covers Highest, S4U and combined legacy
principals, unchanged reruns, refused/ignored principal writes, and a running
legacy principal with uninspectable daemon. That case updates registration but
refuses stop/start; its pending restart resumes only after observed daemon absence.
Actual PS5 focused -BoundaryChild fixture PASS 81823. The initial complete run
75470 passed the nested PS7/Bash/PS5 fixture but its outer registrar test hit the
previously identified inherited-module Get-FileHash lookup issue. The focused
rerun dot-loaded the existing shared initializer before the fixture; no machine
module settings were changed. Separate diagnostic-entry work wraps registrar
initialization; this integration now includes both repairs. No live task, daemon, UAC,
firewall or heavy build was exercised. Public binary/runtime proof remains open.
### Firewall ownership reconciliation (2026-10-02)

OPEN live acceptance. A peer's sanitized inventory contained 47 candidate rules:
39 Allow and 8 Block. None carried an installer/account ownership Group. Only the
current executable's canonical TCP/UDP pair had the full installer description.
Other entries included Windows Query User rules for build/test binaries and an
ambiguous Program=Any allowance. Their names and paths do not establish ownership;
setup must not remove all 47 or report that it did. The actual peer inventory is
not committed; the regression reproduces its shape with synthetic paths.

The existing helper removed every local inbound rule for the selected executable,
including explicit Blocks. This is now refused before any rule writes when an
active inbound Block applies; all Block rules and unrelated policy are preserved.
New canonical rules receive a stable per-account Group. The normal coordinator
passes its original SID through the existing elevated adapter. Obsolete Allow
rules are removed only with matching account Group, full canonical signature and
an absolute airc.exe path. Existing local inbound Allow policy migrates only at the explicitly
selected current executable, including broad Windows prompt allowances. Foreign
Group ownership at that exact executable refuses before writes. No ownership is
inferred from an old build path,
Query User name, or matching display name alone.

Both CheckOnly and apply report owned/current legacy/unowned candidate counts and
preserved rule metadata. The known inventory therefore retains 45 unresolved
candidates while migrating the canonical pair; it is not a clean-inventory claim.
The Program=Any legacy allowance remains an explicit investigation item until its
origin and full port/address policy can be proven. Continuum continues through
its existing AIRC FirewallOnly entry, using this same policy implementation.

Validation: actual PS5 policy and native process adapter fixtures passed (52673
before report-on-check additions; final 66041 includes inventory-shaped cases).
Coverage includes obsolete account-owned path cleanup, other-account preservation,
current canonical migration, all 8 inventory Blocks surviving, all 47 entries
remaining where 45 lack ownership proof, refusal of current Block without writes,
healthy-rerun unresolved counts, literal paths, original SID handoff and failures.
No admin/UAC, live rules, daemon or build was exercised.
### 2026-10-02 - updater IPC uncertainty and running-build freshness (OPEN)

Bigmama reported a supported update returning success with installed build
834d1cd while the running owner was inaccessible. The updater reduced every
failed subprocess ping to false, and its no-op path checked only source and
installed-file revisions. Permission failures could therefore mean stopped,
and a reachable old daemon could survive an already-current file indefinitely.

The repair uses the normal typed IPC status client and the existing legacy
endpoint absence predicate. Only connect NotFound/ConnectionRefused count as
absence; permission errors, RPC timeout, incomplete response and protocol errors
remain failures. Both manual and automatic no-op update paths take the existing
maintenance guard, preserve an absent or matching owner, and use the existing
verified stop/start lifecycle for a reachable stale owner. Explicit installation
retains its start-if-absent policy. No pipe ACL, trust, account, firewall rule or
live daemon was changed by this repair.

A fresh CLI binary passed all three Windows integration tests: real
AccessDenied (an outbound-only fixture pipe, no ACL modification), timeout and
unexpected response fail without installing or spawning; manual/automatic
updates preserve stopped state and matching owner PID; stale owners adopt the
newly built real binary; explicit install starts a missing owner and repeats
successfully. Test daemons use isolated homes. Both the explicit registry gate
and temp-home-only gate were exercised; each emitted its route-isolation receipt
and Windows netstat showed no sockets owned by that daemon PID. Two focused
route-refresh tests preserve the immediate peer-refresh clock for both ordinary
and isolated daemons while allowing automatic listener acquisition only for
ordinary daemons. Full workspace Clippy passed. This binary evidence does
not close public deployment or Bigmama's live outage, which remain OPEN.

CI caught a boundary regression before merge: the first isolation guard stopped
the entire route-refresh task, also disabling intentional stored-loopback peer
reconnection. The correction keeps the immediate/periodic refresh clock, stored
endpoint dialing and delivery snapshots active, while gating only automatic
advertised-endpoint acquisition and relay self-election with the existing
registry isolation policy. No bypass environment or disabled test is added.
The unchanged real-daemon stored-endpoint regression passed locally, alongside
all three updater binary fixtures, both policy-clock tests, and full Clippy.
Fresh rebased-head CI and public deployment remain required.
