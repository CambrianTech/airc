# Projection cache coexistence — df2df8a8

On Windows, current AIRC rebuilt the cambriantech snapshot from format4 to5,
then an old still-running airc join wrote format4 back. A bounded read-only
FileSystemWatcher captured rename from tmp.17204.1182 and the resulting version4;
PID17204 was the join process started October6. Current core8169 embeds AIRCdb7
with format5, so updating the core did not remove the actual old writer.

The shared ProjectionCache now owns paths by projection directory, format version,
read source and room: DIR/vVERSION/daemon-or-store/ROOM.json. Board and wall use
this one implementation. Both existing board test callers now supply their Store
source; there are no per-consumer copies. The old unqualified file is untouched.
Each new namespace rebuilds once from its authoritative transcript. Old and new
clients can then continue independently without taking turns invalidating caches.
Source and embedded version checks remain defense against corrupt/misplaced data;
transcript cursor validation, event order, identity and replay semantics are unchanged.

The generic regression runs two format versions and both read sources alongside
an old writer at the legacy path, then checks each snapshot and resumed cursor
remain independent. Existing corruption, wrong-source/channel, concurrency and
board replay tests retain their responsibility. This is not same-version
singleflight, automatic old-cache collection, or a promised installed speedup.

Validation: Windows shared-cache runs passed nine cache unit tests, five real-daemon
board integration tests, one wall resume integration test, and the existing signed
review replay fixture. Independent review and CI remain required. Live processes,
caches and serving remain untouched. Raw diagnosis is retained in the team-proof
state receipt20261008-cache-writer-collision.md. No installation claimed.
