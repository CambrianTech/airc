# airc reference contract

The trunk is this document. The leaves are `airc` (bash) and `airc.ps1` (PowerShell). Both implementations must conform to the function names, argument shapes, return shapes, preconditions, and postconditions specified here. Implementation details (how bash spawns a background loop vs how PS does it; how each language reads/writes the gist) are leaf concerns.

A bug is one of:
- (a) a leaf drifts from this spec → fix the leaf;
- (b) the spec is wrong → fix the spec, then both leaves.

There is no "the bash and PS versions disagree somewhere unspecified."

## Data shapes

### `address`
```
{
  scope: "localhost" | "lan" | "tailscale",
  addr:  string,            // "127.0.0.1" | "192.168.1.132" | "100.91.51.87"
  port:  integer,
  subnet?: string           // CIDR, only on scope="lan"; lets a peer on the same LAN match
}
```

### `host_info`
The peer-discoverable description of a host. Lives inside the gist envelope.
```
{
  name:        string,        // human-readable handle ("tab-one")
  user:        string,        // gh login of the host
  machine_id:  string,        // stable per-machine id (mac: IOPlatformUUID; linux: /etc/machine-id; win: registry MachineGuid)
  ssh_pubkey:  string,        // base64 single line
  addresses:   address[]      // ordered by host preference; empty list ≡ host can't currently offer any path
}
```

### `host_state`
The host's local handle, returned by `host_publish`, used to drive cleanup.
```
{
  gist_id:    string,
  hb_handle:  opaque,         // bash: pid; ps: job id
  room_name:  string,
  host_info:  host_info       // snapshot at publish time; refreshed by heartbeat
}
```

## Gist primitives

### `gist_publish_room(room_name, host_info) → gist_id`
Creates a kind:room gist for `room_name` with `host_info` and `last_heartbeat = now`.
- Pre: `gh` is authenticated with `gist` scope. No room with this name is currently being hosted by THIS process (but another process's room with the same name is fine — the substrate already allows duplicate-name gists; resolution is by id).
- Post: a gist exists on the gh account containing the envelope; its id is returned. No local files written.
- Idempotent: no — calling twice creates two gists.
- Side effects: one gh API write.

### `gist_heartbeat_start(gist_id, host_info, interval_sec) → hb_handle`
Starts a background loop that, every `interval_sec`, refreshes `host_info.addresses` (re-detecting current localhost/LAN/Tailscale availability) and writes the current envelope (with a fresh `last_heartbeat` and the refreshed addresses) to `gist_id`.
- Pre: `gist_id` was created by `gist_publish_room` (or equivalent — the function does not validate ownership; if the gist isn't ours, gh will 403 and the loop logs a single warning + exits).
- Post: a background worker exists and will continue updating until `gist_heartbeat_stop` is called or the parent process exits.
- Idempotent: no — calling twice spawns two loops, both writing.
- Side effects: periodic gh API writes.

### `gist_heartbeat_stop(hb_handle) → void`
Stops the heartbeat worker. No-op on already-stopped handle.
- Idempotent: yes.

### `gist_delete_quietly(gist_id) → void`
Deletes the gist. Suppresses 404 (already gone) but surfaces other errors (auth, network) to a log line.
- Idempotent: yes — safe to call on a gist that was already deleted.

### `gist_resolve_room(room_name) → host_info | none`
Looks up the most recently updated kind:room gist with this name on the current gh account, returns its `host_info`.
- Pre: `gh` authenticated.
- Returns `none` if no matching gist exists.
- Side effects: one gh API read.

### `gist_list_rooms_by_name(room_name, exclude_id) → gist_id[]`
Returns all kind:room gist ids on the account whose description matches `airc room: {room_name}`, optionally excluding one id.
- Used by `takeover_with_race_check` to spot a concurrent takeover's freshly-published gist.

## Host primitives

### `host_address_set() → address[]`
Enumerates the host's currently usable addresses, in priority order: localhost first, then any LAN interface that has a non-loopback IP, then Tailscale (only if the daemon is up AND the node is signed in — a daemon that says `Logged out` returns NO tailscale entry).
- Pure function of current OS state; no side effects.
- Empty list is impossible — `localhost` is always present.

### `host_machine_id() → string`
Returns a stable per-machine identifier. Same machine, two terminals: same value. Different machines: different values.
- Mac: `ioreg -rd1 -c IOPlatformExpertDevice | awk -F'"' '/IOPlatformUUID/{print $4}'`
- Linux: `/etc/machine-id` (or `/var/lib/dbus/machine-id`)
- Windows: HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid
- Pure; cached after first call.

### `host_publish(room_name) → host_state`
The composite "become host of room_name" operation.
- Calls `host_address_set` and `host_machine_id` to build `host_info`.
- Calls `gist_publish_room` to publish.
- Calls `gist_heartbeat_start` to keep it fresh.
- Registers an exit handler that runs `gist_heartbeat_stop` + `gist_delete_quietly` on graceful shutdown.
- Returns the `host_state` so the caller can shut down explicitly via `host_unpublish` if desired.
- Pre: caller has decided this process should host (i.e. discovery returned no live alternative).

### `host_unpublish(host_state) → void`
Stops the heartbeat and deletes the gist. Used by `cmd_part`. Idempotent.

## Peer primitives

### `peer_pick_address(host_info, my_addresses, my_machine_id) → address | none`
Selects the cheapest reachable address for THIS peer to dial.
- If `host_info.machine_id == my_machine_id` AND `host_info.addresses` contains a `localhost` entry → return that entry.
- Else: among `host_info.addresses` whose `scope == "lan"`, prefer one whose `subnet` contains any of `my_addresses` (same LAN). Return the first such match.
- Else: among `host_info.addresses`, return the first `tailscale` entry.
- Else: return `none`.

### `peer_try_pair(host_info, my_state) → paired_state | unreachable`
Walks `host_info.addresses` in `peer_pick_address` priority order, attempting TCP+SSH handshake against each with a 1.5s connect timeout. First success returns `paired_state`. All failures return `unreachable`.
- This replaces today's "try one IP, fail, fall through to self-heal" pattern. `unreachable` means "no path works" — a much stronger signal than "this one IP didn't work."

## Takeover primitive

### `takeover_with_race_check(stale_gist_id, room_name) → win | join(other_gist_id) | give_up`
The single coherent takeover operation; called by both heartbeat-stale fast-path AND SSH-failure self-heal.
- Sleep a jittered 100–1500 ms (de-synchronize concurrent racers).
- `gist_delete_quietly(stale_gist_id)`.
- `gist_list_rooms_by_name(room_name, exclude_id=stale_gist_id)`. If non-empty, return `join(picked_id)` — another tab won the race; we should join their fresh gist.
- Else return `win` — caller proceeds to `host_publish`.
- Returns `give_up` only if `gh` is unavailable.

## Language-specific leaves

| concept                  | bash                                | PowerShell                                    |
|--------------------------|-------------------------------------|-----------------------------------------------|
| background worker        | `(loop) &` + `$!`                   | `Start-ThreadJob` returning a job             |
| graceful-exit hook       | `trap "..." EXIT INT TERM`          | `Register-EngineEvent PowerShell.Exiting`     |
| JSON parse               | `jq` (with awk fallback)            | `ConvertFrom-Json`                            |
| stable machine id        | `ioreg` / `cat /etc/machine-id`     | registry read via `Get-ItemProperty`          |
| tailscale CLI            | `resolve_tailscale_bin`             | `Resolve-TailscaleBin`                        |

These are implementation details. The function NAMES and CONTRACTS above do not change between languages.

## Tests

Each primitive has a contract test that runs against the actual implementation:
- `gist_*`: hits a real gh test account, asserts state transitions.
- `host_address_set`: asserts `localhost` always present; asserts no `tailscale` entry when daemon reports `Logged out`.
- `host_machine_id`: asserts equal across two calls; asserts non-empty.
- `peer_pick_address`: pure-function test with synthetic inputs covering the same-machine, same-LAN, cross-Tailscale, and `none` cases.
- `takeover_with_race_check`: spawn TWO concurrent calls against the same stale gist id; assert exactly one returns `win` and the other returns `join`.

These tests target the contract, not a specific shell. The same test suite (driven by a thin shell or PS shim that calls the function and checks the return) runs under both leaves.
