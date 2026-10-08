# Daemon descriptor capacity (card 514b13a7)

During Fable M5 prebuilt update recovery on 2026-10-08, the restored daemon
started and briefly answered status, then exited with `unix_accept: Too many
open files (os error 24)`. The initiating SSH shell had soft nofile256 and an
unlimited hard limit. The prior daemon had roughly705 connections. Supported
adoption with an inherited soft4096 limit recovered service; at329seconds it
still reported703 connections. Continuum core80117 was left running.

`airc-daemon::file_capacity` now owns startup descriptor capacity from
`DaemonState::build`, before the event sink/router and mesh begin serving.
The Unix adapter uses the already-resolved rustix dependency's safe process
API. It raises only the soft limit toward4096, capped at the current hard limit;
it never lowers either limit. This reserves capacity without opening files.
The target supplies headroom for the observed mesh, not a claim that arbitrary
mesh growth fits or that descriptor leaks are impossible.

Startup emits a typed daemon_file_capacity diagnostic with previous and actual
soft limits, hard limit and requested target. Restriction or OS refusal warns
and continues; it does not reject a weaker machine or pretend the limit rose.
Windows has no RLIMIT_NOFILE adapter and retains its existing behavior.

The policy contract is tested once with low hard limits, already higher limits
and unlimited limits. A Unix subprocess test lowers only its own soft limit,
executes the real adapter and verifies the result and diagnostic. It avoids
changing the parallel test runner's process limits. Mac tests2/2 pass; Windows
strict daemon all-targets Clippy passes. CI and installed low-limit restart
acceptance remain OPEN until the merged prebuilt is adopted without the manual
shell adjustment. The recovery intervention alone does not close the gap.

Windows CI run37801319391 failed an existing stdin-timeout fixture after the
hook exited successfully and emitted its expected5second timeout diagnostic.
The final duplicate elapsed<15 assertion measured15.0489645seconds. The fixture
also included cold daemon setup in that measurement. Its normal hook now warms
the owned fixture before timing the held-open stdin; its original15second
watchdog still kills/reaps a child that remains running. The redundant strict
post-success timing assertion is removed; production timeout and watchdog
budget are unchanged, success and deadline diagnostic assertions remain.
