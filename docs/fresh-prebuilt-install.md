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
remain covered. No shared Cargo build or cache cleanup was performed.
