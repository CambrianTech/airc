# AIRC Account Mesh Join Contract

**Status:** required contract for rust-rewrite
**Date:** 2026-05-20

## Decision

`airc join` means: enter the account-wide mesh for the current Git/GitHub user
identity, then subscribe this scope to the default channels for the current
context.

It does not mean "join one isolated local room." It does not require a user to
paste an invite when another agent on the same Git/GitHub user account already
has a live mesh. It does not make `#general` and the project channel mutually
exclusive.

The old system's useful behavior was:

- every agent, tab, terminal, and machine on Joel's Git/GitHub account could
  meet in the same `#general`;
- every CambrianTech repo on any of Joel's machines joined the same
  `#cambriantech` room;
- every Ideem repo on any of Joel's machines joined the same `#ideem` room;
- the account identity, not the machine, was the room namespace;
- a single running monitor could surface traffic from all subscribed channels;
- `airc join` was the normal recovery and convergence verb.

The Rust rewrite must preserve that contract while replacing the brittle
Python/shell/GitHub data plane.

## Concepts

### Install And State Layout

AIRC has one public command name: `airc`. The Rust rewrite must not expose a
parallel language-suffixed product surface, require users to remember which
binary is new, or let different wrappers point at different builds.

The install layout is:

```text
~/.airc/
  src/                 source checkout for the installed AIRC build
  worktrees/           managed agent/development worktrees and leases
  accounts/            account-mesh coordinator/cache state
  scopes/              optional per-scope runtime state when needed
```

Project-local `.airc/` directories may exist for project scope state, but they
must not replace the account-wide coordinator under `~/.airc`.

Forbidden install shapes:

- second hidden source checkouts outside `~/.airc/src`;
- parallel worktree roots outside `~/.airc/worktrees`;
- stale language-suffixed binaries or user-facing symlinks;
- wrappers that silently fall back to an older installed binary;
- test-only environment variables as the normal user path.

The installer places a tiny PATH shim at the platform's normal user-bin
location (for example `~/.local/bin/airc` on POSIX). The shim must resolve to
the canonical installed source command in `~/.airc/src/airc` or the canonical
built artifact for that exact source checkout. It must fail loudly if
resolution is ambiguous. It must not copy the full command implementation,
search arbitrary old install locations, or expose a second product command.

### Curl-Install Contract For AI Agents

AIRC is installed into human and AI-operated shells with a curl-style
bootstrap. That is acceptable only if the resulting system is deterministic,
inspectable, and agent-friendly. The installer must treat AI runtimes as
first-class users, not as an afterthought layered on top of an interactive
human terminal.

Required properties:

- one-line install works from a clean machine and leaves one public command:
  `airc`;
- non-interactive shells work without sourcing `.zshrc`, `.bashrc`, or an IDE
  profile;
- `airc version` identifies the exact installed source checkout that will
  execute subsequent commands;
- `airc doctor` distinguishes install failure, PATH failure, source-checkout
  mismatch, stale process state, broken monitor/hook state, transport failure,
  and remote registry/rate-limit failure;
- install registers Claude skills, Codex skills/hooks, and future agent
  adapters through explicit generated state, not hidden runtime fallbacks;
- reinstall/update is idempotent and never leaves an older public command on
  PATH;
- uninstall removes only AIRC-owned install artifacts and managed skills/hooks,
  while reporting any remaining project/account state rather than guessing;
- the installer does not require Python, Node, SSHD, or GitHub for same-machine
  runtime proof;
- GitHub is allowed for fetching source and rare account-mesh rendezvous, not
  for proving local runtime health.

Trust hardening path:

1. For development channels, the install may track a named branch but must
   report branch and commit in `airc version`.
2. For release channels, install should support a pinned tag/SHA mode.
3. Release install should verify a published checksum or signature before
   executing downloaded artifacts.
4. Update should refuse ambiguous local edits unless explicitly told how to
   reconcile them.

The minimum clean-install proof is:

```text
fresh HOME
install
which airc == platform user-bin shim
airc version -> ~/.airc/src at the expected commit
no stale public airc-core / language-suffixed commands
two fresh project scopes run public airc join
both scopes subscribe #general and their inferred project channel
both scopes derive the same identity-namespaced #general RoomId
both scopes use the same account-home #general wire
airc teardown cleans the scope processes
```

For AI agents specifically, a passing install is not enough. Claude must be
able to attach a live monitor using plain `airc join --attach`. Codex must be
able to receive unread events through the installed hook or an equivalent
subscription surface. Both paths must be tested through public `airc` commands,
not absolute source paths or test-only environment variables.

For version 1 there is no legacy runtime surface. Old Python, shell, gist-chat,
and language-suffixed compatibility paths should be deleted when their Rust
replacement exists. If a fresh install would never create a file or symlink,
the runtime should not contain code to keep using it.

### Account Mesh

An account mesh is the discovery namespace shared by all machines, tabs,
terminals, and agents authenticated as the same Git/GitHub user identity. In
the old implementation, GitHub gists accidentally provided this namespace:
"same GitHub account" meant "same canonical channel registry."

Rust must make this explicit. A mesh registry maps:

```text
git user identity + channel name -> channel beacon / route candidates / trust data
```

GitHub may publish that registry for remote bootstrap across the user's
machines. Local state, LAN discovery, Tailscale, relay, WebRTC, Reticulum, and
future adapters may publish or mirror the same registry. Consumers do not see
different semantics per transport.

### Machine-Global Coordinator

Every machine needs one local coordinator/cache per Git/GitHub user identity.
It is the machine's shared memory for account-mesh discovery.

The first tab/terminal/agent on a machine that runs `airc join` may need to pay
the rare remote-registry cost: refresh the GitHub-published account mesh,
publish this machine's signed beacon, and probe direct routes. After that, the
result is machine-global state under `~/.airc`; other local scopes attach to
the coordinator/cache and must not independently hammer GitHub.

This is the hierarchy:

1. in-process state;
2. machine-global coordinator/cache for this Git user identity;
3. direct route health: local, LAN, Tailscale, relay, WebRTC, Reticulum;
4. rare GitHub registry refresh/publish when the machine cache is stale,
   missing, or explicitly repaired.

The coordinator owns debounce/singleflight/backoff. Ten local agents starting
at once should produce at most one remote registry refresh, then all ten attach
to the same machine-local truth.

### Channels

Channels are logical subscriptions inside the account mesh. `#general` is the
common account lobby across all of the user's machines. The inferred org/project
channel, for example `#cambriantech` or `#ideem`, is the work channel shared by
all repos in that organization/project namespace.

A scope can be in many channels at once. The channel set is first-class state,
not a side effect of a "current room" file.

The current/default channel is only the target for short commands like
`airc msg "hi"` when the caller does not specify a channel. It must not be the
only channel the monitor, hooks, or event subsystem can see.

### Join

Bare `airc join` should:

1. Load or create the local identity for this scope.
2. Ask the machine-global coordinator for the current Git/GitHub user
   identity's account mesh. The coordinator uses cached local truth when fresh
   and performs at most one rare remote registry refresh when stale.
3. Infer the project/org channel from the current repository.
4. Subscribe to both:
   - `#general`, unless the user explicitly parted it;
   - the inferred org/project channel, when one exists.
5. Start or attach to the scope's event stream.
6. Monitor all subscribed channels.

`airc join --room X` adds or promotes `#X` as a subscription. It must not make
the process deaf to the other subscribed channels.

`airc part` removes a channel subscription. It does not delete identity or
destroy the whole account mesh.

## Monitor And Hooks

Monitor and hook delivery must consume the same Rust event-subscription
surface:

```text
subscribe(scope, filter = subscribed_channels + event/header filters)
resume_from(scope, cursor, filter = subscribed_channels + event/header filters)
page_recent(scope, filter = subscribed_channels + event/header filters)
```

Using `current_room` as the implicit filter is wrong for monitor and hook
surfaces. It hides `#general` when the current channel is `#cambriantech`, and
it hides project traffic when the current channel is `#general`.

For Continuum, OpenClaw, Hermes, and other consumers, this is the same model:
an activity or room is a channel subscription over the account/grid mesh; typed
events flow through the same replayable event bus.

## Gist Boundary

Gists were never valuable because they were a good chat data plane. They were
valuable because they gave the old system a shared account-level registry that
worked across all of Joel's machines under the same Git/GitHub account.

The Rust boundary is:

- GitHub gist may advertise signed channel beacons and route candidates.
- GitHub gist may help any of the user's machines find the same account mesh.
- GitHub gist must not be required for same-machine traffic.
- GitHub gist must not silently become the live data plane for chat/events.
- GitHub gist must be accessed through the machine-global coordinator, with
  TTL, singleflight, and backoff. Individual tabs, monitors, hooks, and message
  sends do not call GitHub directly.

If GitHub is unavailable, local agents on one machine and peers with already
known direct routes should still communicate. Remote first-contact may wait for
another registry publisher or explicit invite, but that is a discovery problem,
not a different chat model.

Rare GitHub access is allowed for:

- first join on a machine with no fresh account-mesh cache;
- publishing or rotating this machine's signed beacon;
- conservative lease refresh;
- explicit user commands such as invite/list/repair;
- discovering remote machines when no local/LAN/Tailscale/relay path is already
  known.

GitHub access is forbidden for:

- routine chat/event frames;
- monitor polling;
- hook execution;
- per-message delivery;
- status loops;
- every tab independently checking the same registry.

## Tailscale Boundary

Tailscale is a first-class remote-route candidate for the user's own machines.
When `airc join` needs a remote route and Tailscale is installed but logged out
or down, AIRC should trigger or clearly guide `tailscale up` instead of silently
pretending only local routes exist.

Tailscale is reachability, not identity. Peers still authenticate with AIRC
identity keys and trust rotation. Tailscale only gives the route resolver a
direct address between machines in the same tailnet.

The foolproof order is:

1. same process / daemon IPC;
2. same machine local transport;
3. same LAN;
4. Tailscale, with login/up flow if the route is needed;
5. relay / WebRTC / Reticulum for non-tailnet or NAT boundaries;
6. GitHub gist only to publish or discover the account mesh and route beacons.

## Rust Status And Gaps To Close

Already landed in rust-rewrite:

- `SubscriptionSet` with subscribed/default/parted channel state;
- stable `(mesh identity, channel name) -> RoomId` derivation;
- mesh identity resolution and cache;
- `Airc::join_default_context()` for `#general` plus inferred org/project;
- public wrapper seeding Rust account-room subscriptions on bare `airc join`;
- same-machine account local wire sharing;
- POSIX PATH shim at the user-bin command surface, dispatching to
  `~/.airc/src/airc`;
- clean-install/runtime guards that reject stale language-suffixed commands and
  exercise the public shim path;
- public installed runtime proof for Linux/macOS clean-install CI:
  `test/public_installed_runtime_proof.sh` validates `airc` from PATH, rejects
  stale `airc-core`, runs two fresh project scopes through public `airc join`,
  proves they converge on the same account-home `#general` RoomId and wire,
  sends through public `airc msg`, and proves the second scope reads that
  message through the Rust event surface;
- machine-global coordinator/cache under `~/.airc/accounts/<identity>/` with
  typed presence beacons, TTL partitioning, and atomic refresh singleflight;
- `Airc::join` / `Airc::join_default_context()` publish coordinator beacons for
  the joined subscription set;
- durable Rust event reads synchronously replay the local-fs wire into the
  store before querying, so one-shot commands and hooks do not race a
  background tailer.

Current rust-rewrite still has account-mesh gaps:

- CLI `room` language still needs final cleanup so it reads as subscription
  management rather than single-room switching;
- monitor and hook reliability must be proven through public installed
  commands using the same coordinator-backed join path;
- cross-machine account-mesh discovery still needs the rare remote registry
  publisher/refresh path.

Required corrections:

1. Keep `SubscriptionSet` as the runtime source of truth:
   subscribed channels, default channel, parted channels, and stable
   `(mesh identity, channel name) -> RoomId` derivation.
2. Keep `Airc::join_default_context()` wired through the public wrapper and
   clean-install tests.
3. Keep `Airc::join_channel(name)` / `airc join --room X` additive: add/promote
   a channel without removing the rest.
4. Keep monitor and hooks defaulted to all subscribed channels, not current
   room.
5. Keep machine-global coordinator/cache under `~/.airc/accounts/<identity>/`
   as the local source of truth for joined scopes and live channels.
6. Keep account-mesh registry abstraction for `git user identity + channel ->
   beacon`, and wire public monitor/hook commands through it.
7. Keep the data plane selected by route policy: local, LAN, relay, WebRTC,
   Reticulum, etc. Gist remains registry/bootstrap.
8. Add Rust Tailscale discovery/login health: detect installed/down/logged-out,
   surface `tailscale up`, and publish Tailscale route candidates when signed
   in.
9. Add machine-global coordinator/cache keyed by Git/GitHub user identity, with
   TTL, singleflight, and backoff around all remote registry publishers.
10. Keep install/runtime naming collapsed to `airc`, with source under
   `~/.airc/src`,
   worktrees under `~/.airc/worktrees`, and no stale language-suffixed binaries
   or hidden alternate source roots.

## Handoff: Monitor And Hook Proof

The coordinator foundation and join wiring are landed. Claude should not
reimplement `join_default_context`, wrapper seeding, PATH shim, coordinator
locks, or wire replay. The next slice is the automatic delivery surface.

Scope:

- validate `airc join --attach` from a clean installed source, no manual binary
  copy and no test-only environment override;
- prove a second fresh scope sends via public `airc msg` and the first scope's
  monitor emits the inbound event live;
- prove `airc codex-hook user-prompt-submit` reads the same message through the
  Rust subscribed event surface;
- keep all monitor and hook reads scoped to the subscription set, not a
  single current room;
- do not add shell log scraping, gist polling, symlink workarounds, or
  alternate source roots.

Acceptance for the monitor/hook PR:

- `test/public_installed_runtime_proof.sh` or a sibling public-install proof
  starts two fresh scopes, runs bare `airc join`, sends from one, and observes
  the other through monitor or hook delivery without peer pre-seeding and
  without GitHub;
- one-shot event reads remain deterministic through SDK wire replay, not
  sleeps around background tailers;
- the public command remains `airc`; no hidden language-suffixed runtime path
  or alternate source root is introduced.

## Acceptance Tests

The rewrite is not correct until these pass:

- Two fresh agents on any two of Joel's machines, authenticated as the same
  Git/GitHub user, run `airc join` with no pasted invite and both see the same
  `#general`.
- Two agents in CambrianTech repos on any of Joel's machines run `airc join`
  and both see the same `#cambriantech` and `#general`.
- Two agents in Ideem repos on any of Joel's machines run `airc join` and both
  see the same `#ideem` and `#general`.
- One agent sends to `#general`; another whose current/default channel is
  `#cambriantech` receives it through monitor.
- One agent sends to `#cambriantech`; another whose current/default channel is
  `#general` receives it through monitor.
- Codex hook context includes new events from all subscribed channels, with one
  cursor/replay contract.
- Every machine on the same Git/GitHub user account can discover the same
  canonical channel registry through a remote publisher without changing
  channel semantics.
- If Tailscale is needed for a same-account remote machine and is installed but
  down, `airc join` reports or triggers the login/up path before giving up on
  that route.
- If GitHub is rate-limited, same-machine agents with local route discovery
  still communicate.
- Ten local agents starting `airc join` concurrently cause one remote registry
  refresh at most; the other nine attach through the machine-global coordinator.
- Monitor and Codex hooks never invoke GitHub while draining subscribed-channel
  events.
- After deleting old AIRC state, stale language-suffixed binaries, split source
  roots, parallel worktree roots, and old PATH shims, a fresh install produces
  exactly one public command, `airc`, and `airc version` reports the same
  source checkout that the command executes.
- Clean-install CI runs the installed public-command proof on Linux and macOS,
  not just direct `airc-core` cargo tests.
