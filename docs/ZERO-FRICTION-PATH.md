# Zero-Friction Path — the full automation map

**Status**: Design doctrine (companion to card 657ad655 — that card says NEVER; this doc says HOW)
**Premise (Joel)**: "We can automate everything. We have Rust at all layers." Users are
assumed non-technical. Every step a human performs is a bug; this doc assigns each one
its Rust mechanism.

## The target user journey

```
1. ONE command (or one double-click):    curl -fsSL https://get.<domain>/install | sh
2. Binary downloads, signature verifies, lands in a USER-writable dir. Zero prompts.
3. First machine: identity keypair auto-created. A grid of one, working.
4. Second machine: same install — and the machines pair THEMSELVES (see the pairing
   waterfall below). In the common cases the user does NOTHING; the worst case is one
   tap on a machine they already own. The grid appears.
5. Forever after: self-updating, self-healing, zero ceremony. The user never sees
   a compiler, a firewall dialog, an elevation prompt, a port, or a peer spec.
```

The test (from 657ad655): a child installs with one command and the grid simply appears.

## Friction → mechanism map

Every row below is a thing a human did during the 2026-06-10 5090 bring-up. None survive.

| Tonight's friction | Rust mechanism that deletes it | Exists today? | Owner/card |
|---|---|---|---|
| Installed rustup + MSVC BuildTools, compiled from source | **Prebuilt signed release binaries** per platform (win-x64, mac-arm64/x64, linux-x64) built by CI on tag; installer = download + verify hash/signature + run. Source build stays behind `--dev`. | release.yml: NO. Installer rework: #1115 reframed dev-path | NEW: release pipeline card |
| Wrong branch installed (legacy canary vs rust-rewrite), parallel ghost mesh | **Single release channel + protocol-line marker in rendezvous artifacts**; join REFUSES or loudly warns on line mismatch ("this room lives on a newer protocol — updating…" → self-update → retry) | NO (card 01c7320c) | 01c7320c |
| `cargo install` wrote to a dir PATH didn't serve (stale binary ran for hours) | **Self-updating binary**: the daemon checks the release feed, swaps itself atomically in ONE canonical location, restarts. The version-skew banner stops telling humans to run cargo; it becomes a trigger for self-update. | NO (cards 6564fcc6 + this) | 6564fcc6 grows into self-update |
| Manual peer-spec paste (uuid:pubkey over a gist) | **Machines pair THEMSELVES — the pairing waterfall** (Joel: 'don't frustrate users with stupid codes we can pass ourselves'). What must cross is tiny and machine-readable: signed peer record (peer_id, pubkey, endpoints, protocol-line). Any authenticated channel both machines already hold carries it automatically — tonight's irony is that BOTH machines had the same gh auth and agents hand-pasted specs through a gist; airc must do natively what we did manually: publish signed peer-record blobs to the account store, read peers', mutually enrol. See §Pairing waterfall. Codes exist only at the waterfall's bottom. | Account-registry room convergence: YES (rooms only). Peer-record carriage: NO. Mnemonic fallback: YES | NEW card (peer-record auto-exchange) + 625abe6d |
| Manual endpoint exchange + lan-send/lan-listen bisection | **LAN auto-discovery (mDNS/DNS-SD)**: every node announces (peer_id, endpoints, protocol-line) on the local network; resolver collects candidates without any exchange. `mdns-sd` crate, pure Rust, no elevation. | NO | 625abe6d (discovery leg) |
| Routes never formed (peer records have no endpoint) | **Multi-endpoint peer records raced in cost order** (LAN → tailnet → relay), continuous health-checks, automatic failover, relay-by-default forwarding with original-signature preservation | NO — THE routes card | 625abe6d (owner: 5090 agent) |
| Windows Firewall inbound caveat | **Outbound-only participation as a hard guarantee**: listen endpoints are an optimization some nodes advertise, never a requirement. A node that can make ONE outbound TLS/QUIC connection to any reachable peer is fully on the mesh (relay carries the rest). No inbound rule can ever be needed. | Doctrine accepted into 625abe6d design | 625abe6d |
| gh auth as rendezvous dependency (token death, gist scopes) | **Accounts/rendezvous are for SHARING only — never required for your own grid** (Joel directive). A user's machines form a complete grid with zero accounts: phrase-pair + LAN discovery + intra-grid gossip. gh (dev) or a federated zero-knowledge signaling service (user) enters ONLY for cross-grid sharing: joining someone else's room, publishing adapters, public invites. Invite mnemonics encode which rendezvous to use when crossing grids. | NO — design open | NEW card on promotion track |
| UAC / admin service installs / autologon decisions | **User-level everything**: per-user install dir, launchd agent (mac) / `HKCU` Run key or user-mode startup (win) / `systemd --user` (linux). Survives login, not reboot-to-login — acceptable: the mesh self-heals around a sleeping node (625abe6d) and nothing on a user box is a designated hub. | Partially (scope daemons are user-level) | install rework |
| "Binary N commits behind — rebuild" banners aimed at humans | Banner becomes an EVENT: daemon self-updates on a channel (stable for users, canary for dev) and restarts seamlessly; clients reconnect via the session protocol's snapshot-then-live (positron) / cursor resume (airc) — already built for exactly this | Skew detection: YES. Acting on it: NO | self-update card |
| Agent babysitting the whole install | **`airc doctor` doctrine generalized**: the daemon continuously self-diagnoses and FIXES recoverable states (stale sockets, dead routes, version skew) instead of reporting them. Doctor already "proactively fixes recoverable issues" on demand; make it the daemon's idle loop. | Doctor verb: YES. Continuous: NO | NEW small card |

## The pairing waterfall — codes only when nothing else exists

What pairing must exchange: one **signed peer record** per machine — `(peer_id, pubkey,
endpoints[], protocol-line)`, ~200 bytes. Small enough for ANY channel. The waterfall
tries channels in order; the user is involved only when a rung requires it:

1. **Shared authenticated store** (dev: the gh account that machines already hold;
   user: whatever sign-in the platform already gave the machine — Apple/Google/OS
   account, or a continuum account created once). Each machine publishes its signed
   peer record there; all machines subscribed to the store auto-enrol each other and
   gossip endpoint updates. **Zero interaction. This is what the gists were for.**
2. **Same LAN + shared identity anchor** — mDNS finds the candidate; the anchor (from
   rung 1's store, cached) proves same-owner; auto-pair. Zero interaction, works when
   the store is unreachable (offline LAN).
3. **Same LAN, no shared anchor** — mDNS finds the candidate; an EXISTING grid machine
   surfaces "found 'bigmama' on your network — yours? [yes]". **One tap on a machine
   the user already trusts. No codes, nothing typed on the new machine.**
4. **Remote + no common anything** — only here does the 4-word mnemonic / QR appear,
   and it is generated and consumed by machines (user relays it once, by voice or
   camera). This rung is the FALLBACK, never the design center.

The same waterfall pattern governs every other communication point — endpoints
(gossiped inside the grid + republished to the store), room registry (already
store-carried), presence/work-state (mesh-carried once routes exist). Inventory rule:
for each thing that must cross machines, name the payload, then ride the BEST channel
that already exists — never mint a new credential, never show a human a token.

## Order of operations (what unlocks what)

1. **Release pipeline** (binaries + signatures + channels) — unlocks binary-first installer
   AND self-update AND the promotion story (main = stable channel). Smallest slice with
   the largest deletion of friction. The 5090 is the Windows build/test bench.
2. **625abe6d routes** (endpoints, cost-order racing, outbound-only, relay, self-heal) —
   unlocks cross-machine steady state, deletes all manual transport ceremony. In flight.
3. **Self-update + continuous doctor** — deletes the entire version-skew class.
4. **Pairing waterfall rungs 1-3** (peer-record auto-exchange via existing store, mDNS +
   anchor, one-tap LAN adopt) — deletes pairing ceremony outright; the mnemonic survives
   only as rung 4 and for crossing into other grids.
5. **User rendezvous service** — last, because dev-gh works until real users arrive, and
   its design (federated, zero-knowledge) deserves the same review rigor as the economy.

## What this doc does NOT relax

E2E encryption, signature provenance (through relays — forwarders never re-sign),
explicit trust tiers, and the operator's right to a fully manual dev path (`--dev`,
the lan verbs, peer add) all survive. Automation removes ceremony, never consent or
verifiability. `NeedsOperator` (8de61385) remains for the genuinely physical
(hardware, BIOS, purchases) — the doctrine is that list converges toward empty.
