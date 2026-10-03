# AIRC authenticated session acceptance — 2026-10-03

Owner: Codex/Astra (alias routing, PR1512), Fable (session lifetime, PR1511).

Installed baseline93ff on BigMama: ordinary forwarded durable events reach the M5 but acknowledgments are absent. Fable supplied reader receipts for ea25dd62/89313c1d. A supported direct LAN send returned a typed persistence ACK for de8b960e-40c9-4463-b947-d7b85c9d1163, then Fable supplied reader ACK5f663051. Same cached endpoint192.168.1.249:57958. Local OS count:69 established sockets owned by daemon17640; M5 reported56 earlier. These are observations at different times, not matching snapshots.

Source cause reproduced: TLS reverse lookup can index a shared key under an older identity, while signed forward frames name its current identity. ACK unicast used only exact UUID lookup. The real TLS fixture fails baseline with NoActivePeers. PR1512 retains the actual certificate key in PR1511's Session owner, then authorizes the requested peer against that key through PeerKeyRegistry. No new connection table, trust mutation or raw SQL. Unknown, different-key, revoked and key-replaced identities are rejected.

Validation:39/39 transport tests pass, including Fable's replacement test. Scoped and full pre-push workspace clippy/fmt pass. PR1512 head9a5238a7 is stacked on1511 head52175f02. Source review identified an additional1511 hole: aborting only the reader leaves a blocked writer owning its TLS half. Fable was asked to cancel both halves through Session and cover blocked output (review5968325062). No merge approval yet.

Delivery still required: updated source review and CI; supported canary installation on receiver and sender; verify running revision; normal room message persistence plus reader ACK and daemon delivery ledger; repeated recovery must not grow live socket count. Direct send success or passing tests alone do not close this incident. Production remains93ff; Kimi/Continuum serving5944/d8 is preserved.
Update10:41: rebased locally onto1511 writer-cancel head79424c68. Combined transport40/40 passed, including blocked writer. Prior blocking review closed by scoped source approval5968411993. Same authenticated session lookup now also guards duplicate connects and discovery; connected_peers stays a physical-session snapshot to avoid duplicate fanout. Discovery tests95167 active; latest local changes not yet pushed/reviewed/installed.

10:51 validation: discovery suite10/10 passed; combined transport40/40; full workspace all-targets clippy and fmt passed. Shared session lookup covers unicast, duplicate connect and discovery without expanding physical-session fanout. Publication/review and installed acceptance remain pending. PR1511 Windows test lane still pending; no merge performed.

## Installed lifetime fix — 2026-10-03 11:13 UTC

PR1511 merged to canary as a254492203e5cbcad7ad7acf6e9f74dbb92a2f62 after current-head review and all 15 CI checks. Supported `airc update` handle71869 exited0. Fresh CLI version and daemon status both report a2544922 on BigMama. Fable's normal-room receipt03b6e2bf-cbe8-4489-b6b6-77b31b3cbf27 reports the same supported adoption on M5.

Seven OS samples from11:12:21.828Z through11:13:22.383Z each show exactly one established socket to192.168.1.249:57958, with unchanged local port57343 and daemon PID18076. The earlier baseline snapshot had69 sockets. Evidence: C:/Users/joelt/.continuum/state/team-proof-20260921/airc-1511-socket-window.json. This is a bounded stable observation after adoption, not a repeated disconnect stress test or popup-monitor result.

Normal M5 acknowledgment remains unresolved: the fresh delivery ledger showed3 attempts with no ACK; the other two peers had fresh ACKs. Alias routing is NOT installed. Replacement PR1513 (https://github.com/CambrianTech/airc/pull/1513) targets canary at f335c47e3f0d7f313684f46e263ab2efd625de99; PR1512 was automatically closed when its base branch was deleted. Current-head review requested from Fable; CI pending at last check. Do not reuse the earlier9a approval. Next: reviewed/green canary promotion, supported adoption on both endpoints, then ordinary room message plus reader acknowledgment and delivery-ledger acceptance. No current build/install remains; monitor30905 completed. Continuum5944/d8 and Kimi serving state preserved.
