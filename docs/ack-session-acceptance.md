# AIRC authenticated session acceptance — 2026-10-03

Owner: Codex/Astra (alias routing, PR1512), Fable (session lifetime, PR1511).

Installed baseline93ff on BigMama: ordinary forwarded durable events reach the M5 but acknowledgments are absent. Fable supplied reader receipts for ea25dd62/89313c1d. A supported direct LAN send returned a typed persistence ACK for de8b960e-40c9-4463-b947-d7b85c9d1163, then Fable supplied reader ACK5f663051. Same cached endpoint192.168.1.249:57958. Local OS count:69 established sockets owned by daemon17640; M5 reported56 earlier. These are observations at different times, not matching snapshots.

Source cause reproduced: TLS reverse lookup can index a shared key under an older identity, while signed forward frames name its current identity. ACK unicast used only exact UUID lookup. The real TLS fixture fails baseline with NoActivePeers. PR1512 retains the actual certificate key in PR1511's Session owner, then authorizes the requested peer against that key through PeerKeyRegistry. No new connection table, trust mutation or raw SQL. Unknown, different-key, revoked and key-replaced identities are rejected.

Validation:39/39 transport tests pass, including Fable's replacement test. Scoped and full pre-push workspace clippy/fmt pass. PR1512 head9a5238a7 is stacked on1511 head52175f02. Source review identified an additional1511 hole: aborting only the reader leaves a blocked writer owning its TLS half. Fable was asked to cancel both halves through Session and cover blocked output (review5968325062). No merge approval yet.

Delivery still required: updated source review and CI; supported canary installation on receiver and sender; verify running revision; normal room message persistence plus reader ACK and daemon delivery ledger; repeated recovery must not grow live socket count. Direct send success or passing tests alone do not close this incident. Production remains93ff; Kimi/Continuum serving5944/d8 is preserved.
Update10:41: rebased locally onto1511 writer-cancel head79424c68. Combined transport40/40 passed, including blocked writer. Prior blocking review closed by scoped source approval5968411993. Same authenticated session lookup now also guards duplicate connects and discovery; connected_peers stays a physical-session snapshot to avoid duplicate fanout. Discovery tests95167 active; latest local changes not yet pushed/reviewed/installed.

10:51 validation: discovery suite10/10 passed; combined transport40/40; full workspace all-targets clippy and fmt passed. Shared session lookup covers unicast, duplicate connect and discovery without expanding physical-session fanout. Publication/review and installed acceptance remain pending. PR1511 Windows test lane still pending; no merge performed.
