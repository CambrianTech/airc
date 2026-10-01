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
| Fresh-space reserve rejected an already-installed rerun | Windows adapter now uses remaining-work reserves and checks selected build output | **OPEN:** secondary-volume fixture passes, but independent review found system-volume reruns still demand a fresh reserve and Cargo-configured targets are not fully accounted for |
| Existing daemon may retain an old PATH after prerequisites change | Installation must reconcile daemon lifecycle using the supported startup/join path | **OPEN investigation:** portable gh is absent from runtime fallback paths; inspect daemon registry diagnostics and process age before attributing missing enrollment |
| Health command passed while no same-account peers were enrolled | Record explicit account-registry enrollment and two-way peer acknowledgement as mesh acceptance evidence | **OPEN:** zero enrolled peers observed; doctor exit 0 is not proof of the requested connection |
| Windows startup registration failure can remain a warning | Review native startup failure propagation in the shared coordinator | **OPEN audit:** verify promised startup behavior and ensure failure cannot be reported as successful installation |
| Local Rust 1.99 Clippy gate fails in dependency-generated code | Integrate and validate the narrow upstream compiler-compatibility fixes | **VERIFIED locally:** applied Bigmama PR #1469's async-trait 0.1.92 lockfile and doctor closure changes (24b88d0 / 34829dc); fmt and clippy --all-targets -- -D warnings pass on Rust 1.99; CI still required |
| Initial wrong branch | Repair branch is based on origin/canary | Corrected; no commit/PR yet |
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
