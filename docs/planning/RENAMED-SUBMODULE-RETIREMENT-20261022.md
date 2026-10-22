# Retire present submodules after shared registration changes

Card: `2c393644-2586-4e65-9802-9454bcddcca5`.

Installed AIRC `351adeb9fe86` classified twenty clean terminal checkouts as
removable. Supported cleanup retired one and retained nineteen. Net observed
C: free-space increase was 1,004,957,696 bytes, leaving 88,054,681,600 bytes
free, still below deployment admission headroom. No manual deletion, model/KV
eviction, runtime stop or shared Cargo cache change was performed.

The initial diagnosis of duplicate repositories was wrong. The old
`modules/src/workers/vendor/whisper.cpp` name is the actual gitdir of the current
`core/vendor/whisper.cpp` checkout. After the first retirement runs `submodule
deinit --all`, shared superproject registration disappears for sibling worktrees.
Their checkouts still exist, but ordinary `submodule status` reports `-`, so
retirement skips examining them and correctly refuses their remaining stores.
A read-only `git -c submodule.active=. submodule status --recursive` on retained
139c651d finds both actual checkouts at their recorded commits.

The existing retirement owner applies that command-local activity setting to
status and deinit. It does not initialize absent checkouts or persist a new
registration. Truly deinitialized repositories remain unexamined and refused.
The extracted shared `verify_submodule_contents` retains history, tag, stash
and ignored-file protections and adds explicit index/status and linked-worktree
checks: storage registered to another checkout cannot be retired. Recovery refs
and Git worktree/branch deletion protections remain.

The existing Git fixture models lost shared registration with present files,
plus an external linked checkout and dirty index. Prior deinitialized,
custom-ref, reflog, tag and ignored-file tests remain. No parallel cleaner or
harness was added. Formatting/diff checks pass; compilation, tests, strict
Clippy, independent review, publication and installed acceptance remain pending.
Local compiler use is deferred to protect serving/deployment ownership; CI will
provide the required tests before merge.

Raw receipts under `C:/Users/joelt/.continuum/state/team-proof-20260921/` include
`20261022-cleanup-apply.log`, candidate/guard/size JSON inventories, and disk
before/after JSON files. The README links the inspection. The source draft does
not mean remaining space has been recovered.
