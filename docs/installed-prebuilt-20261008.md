# Windows published AIRC prebuilt adoption — 2026-10-08

The ordinary supported command `airc --home C:/Users/joelt/.airc update` consumed
published canary af64b67e033bbee650981db305da007999d1a123 without explicit artifact
input. Invocation-scoped AIRC_DEVELOPER_BUILD=0 prevented compilation. The installer
prepared while service remained available, then completed its own maintenance
handoff. CLI and daemon both reported af64b67e033b; no manual stop/join was used.

Identity e85a5bb3-74f0-4325-87df-7d5f27637063 and the saved Codex coordination brief
(repeat 600000ms) were retained. An explicit resume consumer received that brief
and actionable board/maintenance issues; immediate polling of the same consumer
returned zero bytes. Installed update skill now describes automatic verified
prebuilt discovery and no compiler fallback.

The retained old join PID17204 (started October6) remains alive. A normal board
read created work-board-cache/v5/daemon/cb2e21a1-999a-5a03-a184-df06e4ee7097.json
with version5 alongside the retained legacy root version4 file; no cache was
deleted. This demonstrates installed namespace coexistence and a successful new
consumer read; it does not prove every old writer has subsequently written.

After a normal coordination message, delivery ledger reported peer0121d959
39/41 ACKed, latest3s, RTT152ms, and peer5159a48b 3/3 ACKed, latest27s, RTT2046ms.
These are daemon delivery acknowledgements, not intended-reader consumption.
The existing store-size warning remains (1394MB plus6MB WAL); no vacuum/reset was
performed. Primary source checkout stayed clean on feat/msg-to-peer. Continuum
compiler owner and serving state were untouched. Fresh-machine installation and
Mac fleet adoption remain separate acceptance work.

Raw local receipts under C:/Users/joelt/.continuum/state/team-proof-20260921:
- 20261008-airc-af64-prebuilt-update.log
- 20261008-airc-af64-verification.txt
- 20261008-airc-af64-delivery.txt
- 20261008-airc-af64-board-read.txt
- 20261008-airc-af64-resume-repeat.txt (empty by expected cadence suppression)
