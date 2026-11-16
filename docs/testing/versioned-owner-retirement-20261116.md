# Versioned machine endpoint retirement, 2026-11-16

Status: OPEN. Owner: Codex.

BigMama had two live daemons reporting the same durable peer e85a5bb3-74f0-4325-87df-7d5f27637063, IPC 6 and build cf964657: PID 27448 on airc-machine-9779141bff91efa0-v5.sock and PID 35280 on the canonical v6 endpoint. Both held network listeners. Ordinary room messages were absent from the M5 store while a direct TLS send was persisted. The duplicate owner is a verified fault; its causal contribution to missing messages still requires the live comparison.

Manual interventions: status against each exact endpoint, canonical stop/join preserving state, and public update --adopt-installed with both scope and machine homes. Adoption reported current but did not retire the old owner. Public stop correctly refused the noncanonical endpoint.

Diagnosis: legacy_socket_paths_in enumerated only the old daemon-v6.sock locations, omitting versioned machine endpoints. The fix derives a bounded list of earlier versions under the exact canonical account socket's directory. Existing updater identity/protocol verification and graceful, process-pinned shutdown remain the authority. Unknown owners and genuinely old IPC protocols are not assumed safe to stop.

Regression: candidate composition includes the observed v5 endpoint and excludes the canonical endpoint and isolated accounts. Existing migration tests cover differing identities/protocols and uncertain inspection. Independent source review approved the narrow change conditionally on tests and live adoption.

Acceptance still required: public adoption retires PID 27448, canonical service survives with unchanged identity/key and stored messages, ordinary messages arrive on the M5 and Fable replies. Keep this case open until the supported path produces that receipt; unit tests alone do not close it.
Live repair receipt, 2026-11-16T17:03Z: reviewed local maintenance candidate SHA256 69E9AD65718CBEE1CCA8A75AB93DE9B86C770E98CB51B0A91A90D2EB0A3C2BF2 ran public update --adopt-installed. It retired the verified v5 endpoint in 346 ms (exit 0). PID 27448 exited; canonical PID 35280 remained running on installed cf964657 with peer e85a5bb3 unchanged. Prior stored message 13d6d8b6 remained present. Ordinary publish f40b7cd3-fa46-4a47-836c-a72c6bcb5d29 then appeared in the M5 store; Fable explicitly acknowledged that event live at 17:03Z. This closes the observed contact outage after duplicate retirement, not all identity/feed defects. The maintenance candidate was built from the reviewed working diff on cf964657 and was not installed as the daemon. Permanent release integration and recurrence observation remain OPEN.
