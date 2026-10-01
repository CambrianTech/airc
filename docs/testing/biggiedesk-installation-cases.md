# BIGGIEDESK fresh installation: open acceptance cases

Owner: BIGGIEDESK Codex for installer changes; BIGMAMA Codex for SOS and independent review.
All cases remain OPEN until the normal installation exercises the repair end to end.
This is the set known to BIGMAMA, not a claim that BIGGIEDESK's intervention inventory is complete.

| Case | Observed intervention or investigation | Repository repair and regression | End-to-end status |
| --- | --- | --- | --- |
| SOS room collision | BIGMAMA ran `airc join sos`; it selected a mesh room while the other machine used the recovery gist. | CLI rejects reserved names before bootstrap and resolved default/name/UUID writes; two regression tests in commands.rs. | OPEN: PR1469; installed CLI unchanged. |
| Recovery starvation | Repeated `airc sos watch/send` refused at30/30 despite nominal SOS reserve. Source investigation found conflicting CLI/library reservation policies. | CLI uses library GhBudget; explicit Recovery class; shared-budget saturation/window/backoff regression in governor.rs. | OPEN: PR1469; installed governor unchanged. |
| Fresh-toolchain compilation | Local Rust1.96 checks passed; clean CI Rust1.99 rejects async-trait0.1.89 generated must_use attributes. Inspected CI log and dependency macro source. | Cargo.lock upgrades only async-trait to0.1.92, whose upstream release fixes this diagnostic. Existing full CI retains deny-warnings. | OPEN: local clippy passes; fresh CI and installer build still required. |
| Build Tools disk failure | BIGGIEDESK reported VS exit0x80070070, C:5.6GB free and D:4.2TB; installer gave generic Modify advice. | BIGGIEDESK owns installer error classification and supported volume planning; reviewable patch/test receipt pending. | OPEN: no successful supported install receipt. |
| Invisible consent/auth | BIGGIEDESK reported gh MSI hidden UAC; gh authentication later completed manually. | BIGGIEDESK must inventory exact interventions and encode required detection, visible consent, waiting and resumption in existing installer modules. | OPEN: preexisting authenticated state is not proof this path works. |

Upstream compiler compatibility evidence: [async-trait0.1.92 release](https://github.com/dtolnay/async-trait/releases/tag/0.1.92).

For every additional intervention, record the command/action, failure, cause,
installer implementation, regression and actual end-to-end receipt. Label
prerequisites skipped because they were manually installed. Do not erase working
state merely to manufacture a clean-install claim. SOS acknowledgements do not
prove the AIRC mesh is installed or connected.
