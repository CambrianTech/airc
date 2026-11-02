# Published installation without a compiler

Card fcec2452-c34a-430e-82d2-292779a5f2ff closes the AIRC dependency in
Continuum's fresh prebuilt installation (card68a33e89).

`install.sh` and `install.ps1` select a verified published executable by default.
The same `scripts/release-artifact.sh` preparation owner serves the updater's
existing prepare/install transaction. It verifies the archive checksum before
reading its sole executable member, and the coordinator verifies the executable
revision before installation or maintenance. Missing publication is an error,
never a compilation fallback. Existing operator-stop/adoption behavior remains
owned by the updater and installer transaction.

The existing release publisher produces stable version assets and immutable
`canary-<full revision>` prereleases with the same platform/checksum contract.
Unpublished feature revisions wait for publication or require the contributor's
explicit `--developer-build` (`-DeveloperBuild` in PowerShell). The environment
equivalent `AIRC_DEVELOPER_BUILD=1` also propagates across native installer
sessions and allows an explicitly selected source-building updater.

Product preparation does not provision Rust/MSVC or choose a Cargo cache.
Git/Bash, GitHub CLI and normal platform identity prerequisites still apply.
Checksum integrity from the publisher does not imply platform code signing;
the release workflow's existing optional signing policy is unchanged.

The existing install destination fixture now exercises valid publication,
checksum rejection, absent publication and mismatched executable revision.
These are isolated fixtures, not installed adoption receipts. No live daemon,
join owner or serving process was modified during implementation. CI artifacts,
fresh platform installation and guarded update/adoption remain acceptance work.

Local validation (2026-10-08): install destination/publication and actual prepare
fixture passed; standalone update-handoff fixture passed 6 tests, 2 ignored,
in 10.55s; Windows setup-path, PowerShell 5.1 setup-bridge/storage-plan passed.
Bash syntax, PowerShell parsing and diff whitespace checks passed. The source
handoff fixture explicitly selects developer mode so its Cargo failure scenarios
remain covered. Cargo formatting and strict all-target Clippy passed (49.16s);
the coordinated shared compiler slot was released, and no cache cleanup occurred.
Both public wrappers require the published-artifact helper when selecting a
compatible source checkout; existing source-acquisition and native setup fixtures
passed again after that compatibility check.

CI follow-up: macOS lacked sha256sum in the publication fixture; fixture checksum
creation now uses the same available sha256sum/shasum choice as production. The
Windows PS5 job installed and observed a Medium-token daemon successfully, then
public Stop timed out waiting for that daemon to exit. This remains a real
failure; the existing fixture now prints its bounded owned-daemon log on teardown
failure without suppressing the Stop or process-exit check. The timeout's daemon
phase is not yet established. Local normal publication fixture and PowerShell
syntax checks pass; native macOS fallback and Windows acceptance await CI.

Concurrent source review found an independent lost-Stop race: registry startup
created its shutdown future only after asynchronous LAN/store initialization,
although the IPC listener could already accept Stop. The registry join could then
wait forever for a notification that had no subscriber. The daemon now retains
an owned shutdown future created before spawning registry startup. The existing
registry shutdown test covers both steady state and a deterministically delayed
initialization receiving Stop before the loop starts. Admitted registry writes
still drain normally. This source finding is not proof of the earlier CI timeout's
specific cause; the failed-job evidence did not include the daemon's final phase.

Race regression passed locally: registry_refresh::tests::run_loop_exits_on_shutdown
1/1 in 0.24s (both initialization and steady-state scenarios), strict all-target
Clippy 7.68s and formatting passed. Before this runtime change, the diagnostic-only
94c91b8 CI Windows PS5 rerun passed installation and Stop, confirming the earlier
failure is intermittent rather than proving the source race was its cause.
