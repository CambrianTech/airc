"""Codex hook cutover guards.

These are intentionally static: the failure mode was shell entry points
quietly routing Codex hooks back through Python after the Rust hook
existed. The contract here is simple enough to pin directly.
"""

from pathlib import Path
import unittest


REPO = Path(__file__).resolve().parent.parent


class CodexRustCutoverTests(unittest.TestCase):
    def test_runtime_codex_hook_dispatches_to_airc_rs_not_python(self):
        source = (REPO / "lib/airc_bash/cmd_status.sh").read_text(encoding="utf-8")
        start = source.index("cmd_codex_hook()")
        end = source.index("cmd_codex_start()", start)
        body = source[start:end]

        self.assertNotIn("airc_core.codex_hook", body)
        self.assertIn("airc-rs is required for codex-hook", body)
        self.assertIn("codex-hook user-prompt-submit", body)

    def test_installer_uses_rust_codex_hook_installer(self):
        source = (REPO / "install.sh").read_text(encoding="utf-8")
        start = source.index("_install_airc_codex_hooks()")
        end = source.index("if command -v codex", start)
        body = source[start:end]

        self.assertNotIn("airc_core.codex_install", body)
        self.assertIn('codex-hook install-hooks --codex-home "$HOME/.codex"', body)
        self.assertIn("airc-rs not found", body)

    def test_installer_builds_airc_rs_before_hook_registration(self):
        source = (REPO / "install.sh").read_text(encoding="utf-8")
        build_pos = source.index("_install_airc_rs_binary")
        hook_pos = source.index("_install_airc_codex_hooks()")

        self.assertLess(build_pos, hook_pos)
        self.assertIn("cargo build --release -p airc-cli", source)
        self.assertIn('ln -sf "$built" "$BIN_DIR/airc-rs"', source)
        self.assertIn('cp -f "$built" "$BIN_DIR/airc-rs.exe"', source)

    def test_installer_no_longer_bootstraps_python_runtime(self):
        source = (REPO / "install.sh").read_text(encoding="utf-8")

        self.assertNotIn("python3 -m venv", source)
        self.assertNotIn("pip install", source)
        self.assertNotIn("cryptography is not importable", source)
        self.assertNotIn("${AIRC_PYTHON:-python3}", source)

    def test_doctor_no_longer_requires_python_runtime(self):
        source = (REPO / "lib/airc_bash/cmd_doctor.sh").read_text(encoding="utf-8")

        self.assertNotIn('_doctor_probe "python3"', source)
        self.assertNotIn("_doctor_probe_cryptography", source)
        self.assertNotIn("AIRC_PYTHON", source)

    def test_integration_suite_no_longer_runs_python_unit_gate(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")

        self.assertNotIn("scenario_python_units", source)
        self.assertNotIn("python units:", source)

    def test_integration_local_bearer_probe_uses_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("requires_local_pair_bearer_or_skip()")
        end = source.index("# Reap any orphan room gists", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" bearer kinds', body)
        self.assertNotIn("airc_core.bearer_resolver", body)
        self.assertNotIn("python3", body)

    def test_integration_identity_scaffold_uses_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scaffold_identity()")
        end = source.index("# airc send from a given home.", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" identity bootstrap-ed25519', body)
        self.assertNotIn("airc_core.identity", body)
        self.assertNotIn("AIRC_PYTHON", body)
        self.assertNotIn("python3", body)

    def test_integration_top_level_config_edits_use_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        queue_start = source.index("scenario_queue()")
        queue_end = source.index("scenario_status()", queue_start)
        queue_body = source[queue_start:queue_end]
        teardown_start = source.index("airc nick post-sanitization")
        teardown_end = source.index("cleanup_all", teardown_start)
        teardown_body = source[teardown_start:teardown_end]

        self.assertIn('"$(airc_rs_bin)" config get --config /tmp/airc-it-q-j/state/config.json host_target', queue_body)
        self.assertIn('"$(airc_rs_bin)" config set --config /tmp/airc-it-q-j/state/config.json', queue_body)
        self.assertIn('"$(airc_rs_bin)" config get --config "$home/state/config.json" name', teardown_body)
        self.assertNotIn("python3 -c", queue_body)
        self.assertNotIn("python3 -c", teardown_body)

    def test_integration_tabs_peer_record_probe_uses_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scenario_tabs()")
        end = source.index("scenario_scope()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" config get --config "$peer_file" airc_home', body)
        self.assertNotIn("python3 -c", body)

    def test_integration_status_outage_probe_uses_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scenario_status()")
        end = source.index("scenario_auth_failure()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" config get --config /tmp/airc-it-s-j/state/config.json host_target', body)
        self.assertIn('"$(airc_rs_bin)" config set --config /tmp/airc-it-s-j/state/config.json --key host_target', body)
        self.assertNotIn("python3 -c", body)

    def test_integration_heartbeat_gist_probe_uses_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scenario_heartbeat()")
        end = source.index("scenario_bounce()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" gist get .host.name', body)
        self.assertNotIn("python3 -c", body)

    def test_heartbeat_gist_edits_select_canonical_filename(self):
        source = (REPO / "lib/airc_bash/cmd_connect.sh").read_text(encoding="utf-8")

        self.assertIn('"$(airc_rs_bin)" gh patch-gist-file', source)
        self.assertIn('--filename "airc-room-${room_name}.json"', source)
        self.assertIn('--filename "airc-room-${_hb_room}.json"', source)

    def test_stale_host_reexec_preserves_takeover_env(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        start = source.index("_reexec_into()")
        end = source.index("# Stale-host self-heal", start)
        body = source[start:end]

        self.assertIn('local _name="${AIRC_NAME:-}"', body)
        self.assertIn('${AIRC_ADOPT_GIST:+AIRC_ADOPT_GIST="$AIRC_ADOPT_GIST"}', body)
        self.assertIn('${AIRC_HEARTBEAT_SEC:+AIRC_HEARTBEAT_SEC="$AIRC_HEARTBEAT_SEC"}', body)
        self.assertIn('${AIRC_HEARTBEAT_STALE:+AIRC_HEARTBEAT_STALE="$AIRC_HEARTBEAT_STALE"}', body)

    def test_integration_quit_config_probes_use_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scenario_quit()")
        end = source.index("scenario_platform_adapters()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" config get --config "$home/config.json" name', body)
        self.assertIn('"$(airc_rs_bin)" config get-path --config "$home/config.json" .identity.pronouns', body)
        self.assertIn('"$(airc_rs_bin)" config has-key --config "$home/config.json" host_target', body)
        self.assertNotIn("python3 -c", body)

    def test_integration_away_status_probes_use_airc_rs(self):
        source = (REPO / "test/integration.sh").read_text(encoding="utf-8")
        start = source.index("scenario_away()")
        end = source.index("scenario_quit()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" config get-path --config "$home/config.json" .identity.status', body)
        self.assertIn('"$(airc_rs_bin)" config get-path --config "$home/config.json" .identity.status "(absent)"', body)
        self.assertNotIn("python3 -c", body)

    def test_uninstaller_uses_rust_codex_hook_uninstaller(self):
        source = (REPO / "uninstall.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.codex_install", source)
        self.assertIn('codex-hook uninstall-hooks --codex-home "$HOME/.codex"', source)

    def test_codex_start_uses_rust_detach_adapter(self):
        source = (REPO / "lib/airc_bash/cmd_status.sh").read_text(encoding="utf-8")
        start = source.index("cmd_codex_start()")
        body = source[start:]

        self.assertNotIn("airc_core.codex_start", body)
        self.assertIn('"$_airc_rs" codex-start', body)
        self.assertIn("airc-rs is required for codex-start", body)

    def test_runtime_identity_helpers_use_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        resolver_start = source.index("airc_rs_bin()")
        resolver_end = source.index("airc_client_id()", resolver_start)
        client_start = source.index("airc_client_id()")
        client_end = source.index("get_config_val_in()", client_start)
        human_start = source.index("humanhash()")
        human_end = source.index("sign_message()", human_start)

        body = source[client_start:client_end] + source[human_start:human_end]

        self.assertNotIn("airc_core.client_id", body)
        self.assertNotIn("airc_core.humanhash", body)
        self.assertLess(
            source.index('"$_root/target/debug/airc-rs"', resolver_start, resolver_end),
            source.index("command -v airc-rs", resolver_start, resolver_end),
        )
        self.assertIn('"$(airc_rs_bin)" client-id', body)
        self.assertIn('"$(airc_rs_bin)" humanhash "$input"', body)

    def test_lan_ip_probe_uses_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        start = source.index("get_host()")
        end = source.index("resolve_tailscale_bin()", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" lan-ip', body)
        self.assertNotIn("AIRC_PYTHON", body)
        self.assertNotIn("socket.socket", body)

    def test_airc_startup_does_not_require_python(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        startup = source[: source.index("# Issue #341 follow-up")]

        self.assertNotIn("AIRC_PYTHON", startup)
        self.assertNotIn("requires a working python", startup)
        self.assertNotIn("python3 --version", startup)

    def test_airc_has_no_python_debug_runtime_path(self):
        source = (REPO / "airc").read_text(encoding="utf-8")

        self.assertNotIn("debug-pythonpath", source)
        self.assertNotIn("AIRC_PYTHON", source)
        self.assertNotIn("python3", source)

    def test_quick_message_roster_uses_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        start = source.index('if [ "$qm_owner" = "*roster*" ]; then')
        end = source.index("local dispatched=0 skipped=0", start)
        body = source[start:end]

        self.assertIn('"$(airc_rs_bin)" recent-senders', body)
        self.assertNotIn("AIRC_PYTHON", body)
        self.assertNotIn("import json", body)

    def test_scope_repair_uses_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        start = source.index("ensure_init()")
        end = source.index("# config CRUD via airc-rs", start)
        body = source[start:end]

        self.assertNotIn("airc_core.scope_repair", body)
        self.assertIn(
            '"$(airc_rs_bin)" --home "$AIRC_WRITE_DIR" scope repair-config',
            body,
        )

    def test_collaboration_helpers_use_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "lib/airc_bash/cmd_status.sh",
                REPO / "lib/airc_bash/cmd_send.sh",
                REPO / "lib/airc_bash/cmd_identity.sh",
                REPO / "lib/airc_bash/cmd_rooms.sh",
                REPO / "lib/airc_bash/cmd_doctor.sh",
            ]
        )

        self.assertNotIn("airc_core.collaboration", combined)
        self.assertIn('"$(airc_rs_bin)" collaboration status', combined)
        self.assertIn('"$(airc_rs_bin)" collaboration send-warning', combined)
        self.assertIn('"$(airc_rs_bin)" collaboration whois-fallback', combined)
        self.assertIn('"$(airc_rs_bin)" collaboration peers', combined)
        self.assertIn('"$(airc_rs_bin)" collaboration prune-peers', combined)
        self.assertIn('"$(airc_rs_bin)" collaboration doctor', combined)

        rooms = (REPO / "lib/airc_bash/cmd_rooms.sh").read_text(encoding="utf-8")
        start = rooms.index("cmd_peers()")
        body = rooms[start:]
        self.assertNotIn("AIRC_PYTHON", body)
        self.assertNotIn("python", body.lower())

        invite_start = rooms.index("_cmd_invite_human()")
        invite_end = rooms.index("cmd_invite()", invite_start)
        invite_body = rooms[invite_start:invite_end]
        self.assertIn("config get-channel-gist", invite_body)
        self.assertNotIn("AIRC_PYTHON", invite_body)

    def test_inbox_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_status.sh").read_text(encoding="utf-8")
        start = source.index("cmd_inbox()")
        end = source.index("cmd_codex_hook()", start)
        body = source[start:end]

        self.assertNotIn("airc_core.inbox", body)
        self.assertIn('"$(airc_rs_bin)" log inbox-reset', body)
        self.assertIn("log inbox-read", body)

    def test_hygiene_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_hygiene.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.hygiene", source)
        self.assertIn('"$(airc_rs_bin)" hygiene "$@"', source)

    def test_knock_crypto_uses_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "lib/airc_bash/cmd_knock.sh",
                REPO / "lib/airc_bash/cmd_approve.sh",
            ]
        )

        self.assertNotIn("airc_core.knock_crypto", combined)
        self.assertIn('"$(airc_rs_bin)" knock gen-keys', combined)
        self.assertIn('"$(airc_rs_bin)" knock encrypt-for-knocker', combined)
        self.assertIn('"$(airc_rs_bin)" knock decrypt-from-approver', combined)
        self.assertIn('"$(airc_rs_bin)" knock approval-field', combined)
        self.assertIn('"$(airc_rs_bin)" knock identity-json', combined)
        self.assertIn('"$(airc_rs_bin)" knock extract-knocker-pub', combined)
        self.assertIn('"$(airc_rs_bin)" knock extract-approval', combined)
        self.assertNotIn("AIRC_PYTHON", (REPO / "lib/airc_bash/cmd_knock.sh").read_text(encoding="utf-8"))

    def test_iso_to_epoch_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/platform_adapters.sh").read_text(encoding="utf-8")
        start = source.index("iso_to_epoch()")
        end = source.index("# MSYS / Git Bash path conversion", start)
        body = source[start:end]

        self.assertNotIn("airc_core.datetime", body)
        self.assertIn('"$(airc_rs_bin)" iso-to-epoch "$ts"', body)

    def test_log_append_and_rotate_use_airc_rs(self):
        send = (REPO / "lib/airc_bash/cmd_send.sh").read_text(encoding="utf-8")
        connect = (REPO / "lib/airc_bash/cmd_connect.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.log_append", send)
        self.assertIn('"$(airc_rs_bin)" log append --path "$MESSAGES"', send)
        self.assertNotIn("airc_core.log rotate", connect)
        self.assertIn('"$(airc_rs_bin)" log rotate --path "$_hb_messages"', connect)

    def test_bearer_state_uses_airc_rs(self):
        doctor = (REPO / "lib/airc_bash/cmd_doctor.sh").read_text(encoding="utf-8")
        status = (REPO / "lib/airc_bash/cmd_status.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.bearer_state", doctor + status)
        self.assertIn('"$(airc_rs_bin)" bearer-state "$state_file"', doctor)
        self.assertIn('"$(airc_rs_bin)" bearer-state --summary "$bearer_state"', status)

    def test_logs_render_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_status.sh").read_text(encoding="utf-8")
        start = source.index("cmd_logs()")
        end = source.index("cmd_inbox()", start)
        body = source[start:end]

        self.assertNotIn("airc_core.logs", body)
        self.assertIn('"$(airc_rs_bin)" log "$@"', body)

    def test_gistparse_runtime_paths_use_airc_rs(self):
        files = [
            REPO / "airc",
            REPO / "lib/airc_bash/mesh.sh",
            REPO / "lib/airc_bash/cmd_connect.sh",
        ]
        for path in files:
            source = path.read_text(encoding="utf-8")
            self.assertNotIn("airc_core.gistparse", source, path)
        combined = "\n".join(path.read_text(encoding="utf-8") for path in files)
        self.assertIn('"$(airc_rs_bin)" gist get .airc', combined)
        self.assertIn('"$(airc_rs_bin)" gist gist-content', combined)
        self.assertIn('"$(airc_rs_bin)" gist get .last_heartbeat', combined)

    def test_generic_config_runtime_paths_use_airc_rs(self):
        files = [
            REPO / "airc",
            REPO / "lib/airc_bash/cmd_rename.sh",
        ]
        combined = "\n".join(path.read_text(encoding="utf-8") for path in files)

        forbidden = [
            "airc_core.config get_name",
            "airc_core.config get ",
            "airc_core.config set ",
            "airc_core.config set_name",
            "airc_core.config unset_keys",
            "airc_core.config read_parted",
            "airc_core.config record_parted",
            "airc_core.config clear_parted",
            "airc_core.config set_host_block",
        ]
        for needle in forbidden:
            self.assertNotIn(needle, combined)

        self.assertIn('"$(airc_rs_bin)" config get-name', combined)
        self.assertIn('"$(airc_rs_bin)" config set ', combined)
        self.assertIn('"$(airc_rs_bin)" config read-parted', combined)

    def test_rename_collision_scan_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_rename.sh").read_text(encoding="utf-8")

        self.assertNotIn("AIRC_PYTHON", source)
        self.assertIn('"$(airc_rs_bin)" identity rename-collision', source)

    def test_host_block_config_write_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_connect.sh").read_text(encoding="utf-8")
        self.assertNotIn("airc_core.config set_host_block", source)
        self.assertIn('"$(airc_rs_bin)" config set-host-block', source)

    def test_daemon_scope_id_does_not_use_python(self):
        source = (REPO / "lib/airc_bash/lib_daemon_detect.sh").read_text(encoding="utf-8")
        start = source.index("airc_daemon_scope_id()")
        end = source.index("airc_daemon_service_name_for_scope()", start)
        body = source[start:end]

        self.assertNotIn("python3 -c", body)
        self.assertIn('"$(airc_rs_bin)" daemon-scope-id "$target_scope"', body)

    def test_doctor_rate_limit_json_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_doctor.sh").read_text(encoding="utf-8")
        start = source.index("airc doctor --health -- live bus health")
        end = source.index("# ── gh request governor", start)
        body = source[start:end]

        self.assertNotIn("python3 -c", body)
        self.assertIn('"$(airc_rs_bin)" gist get .resources.core.remaining', body)
        self.assertIn('"$(airc_rs_bin)" gist get .resources.core.limit', body)

    def test_whois_pretty_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_identity.sh").read_text(encoding="utf-8")
        start = source.index("_whois_pretty()")
        end = source.index("# cmd_kick extracted", start)
        body = source[start:end]

        self.assertNotIn("python3 <<", body)
        self.assertIn('"$(airc_rs_bin)" identity pretty', body)

    def test_kick_peer_ssh_pub_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_kick.sh").read_text(encoding="utf-8")

        self.assertNotIn("AIRC_PYTHON", source)
        self.assertIn('"$(airc_rs_bin)" identity peer-ssh-pub', source)

    def test_identity_local_state_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_identity.sh").read_text(encoding="utf-8")

        self.assertNotIn("AIRC_PYTHON", source)
        self.assertIn('"$(airc_rs_bin)" identity session-file', source)
        self.assertIn('"$(airc_rs_bin)" identity default-work-name', source)
        self.assertIn('"$(airc_rs_bin)" identity read-work-name', source)
        self.assertIn('"$(airc_rs_bin)" identity write-work-session', source)
        self.assertIn('"$(airc_rs_bin)" identity nudge-needed', source)
        self.assertIn('"$(airc_rs_bin)" identity show-config', source)
        self.assertIn("identity set-config --config", source)
        self.assertIn('"$(airc_rs_bin)" identity link-config', source)
        self.assertIn('"$(airc_rs_bin)" identity import-continuum', source)
        self.assertIn('"$(airc_rs_bin)" identity continuum-handle', source)
        self.assertIn('"$(airc_rs_bin)" identity push-continuum', source)

    def test_lane_registry_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_lane.sh").read_text(encoding="utf-8")

        self.assertNotIn("AIRC_PYTHON", source)
        self.assertIn('"$(airc_rs_bin)" worktree-lane abs-path', source)
        self.assertIn('"$(airc_rs_bin)" worktree-lane slug', source)
        self.assertIn('"$(airc_rs_bin)" worktree-lane record', source)
        self.assertIn('"$(airc_rs_bin)" worktree-lane list', source)
        self.assertIn('"$(airc_rs_bin)" worktree-lane find', source)

    def test_queue_card_core_uses_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "lib/airc_bash/cmd_queue.sh",
                REPO / "lib/airc_bash/cmd_queue_card.sh",
                REPO / "lib/airc_bash/cmd_queue_close_merged.sh",
                REPO / "lib/airc_bash/cmd_queue_plan.sh",
                REPO / "lib/airc_bash/cmd_queue_steward.sh",
            ]
        )

        self.assertIn('"$(airc_rs_bin)" queue-card body', combined)
        self.assertIn('"$(airc_rs_bin)" queue-card mutate-body', combined)
        self.assertIn('"$(airc_rs_bin)" queue-card claim-fields', combined)
        self.assertIn('"$(airc_rs_bin)" queue-card dispatch-message', combined)
        self.assertIn("queue-card adopt-body", combined)
        self.assertIn('"$(airc_rs_bin)" queue-card nudge-summary', combined)
        self.assertIn('"$(airc_rs_bin)" queue-card nudge-card-meta', combined)
        self.assertNotIn("AIRC_PYTHON", (REPO / "lib/airc_bash/cmd_queue_card.sh").read_text(encoding="utf-8"))
        self.assertIn("queue-card list", combined)
        self.assertIn("queue-card stale", combined)
        self.assertIn("queue-card next", combined)
        self.assertIn("queue-card pongs", combined)
        self.assertIn("queue-card availability", combined)
        self.assertIn("queue-card review-refs", combined)
        self.assertIn("queue-card pr-meta", combined)
        self.assertIn("queue-card staleness-analyze", combined)
        self.assertIn("queue-card close-merged-meta", combined)
        self.assertIn("queue-card close-merged-refs", combined)
        self.assertIn("queue-card card-status", combined)
        self.assertIn("queue-card plan", combined)
        self.assertIn("queue-card steward", combined)
        self.assertNotIn("AIRC_PYTHON", (REPO / "lib/airc_bash/cmd_queue_plan.sh").read_text(encoding="utf-8"))
        self.assertNotIn("AIRC_PYTHON", (REPO / "lib/airc_bash/cmd_queue_steward.sh").read_text(encoding="utf-8"))

    def test_message_crypto_helpers_use_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "airc",
                REPO / "lib/airc_bash/cmd_send.sh",
                REPO / "lib/airc_bash/cmd_connect.sh",
            ]
        )

        forbidden = [
            "airc_core.identity sign-ed25519",
            "airc_core.identity bootstrap-ed25519",
            "airc_core.identity bootstrap --dir",
            "airc_core.identity peer_pub",
            "airc_core.envelope wrap",
            "json.load(sys.stdin).get(\"kind\"",
        ]
        for needle in forbidden:
            self.assertNotIn(needle, combined)

        self.assertIn('"$(airc_rs_bin)" identity sign-ed25519', combined)
        self.assertIn('"$(airc_rs_bin)" identity bootstrap-ed25519', combined)
        self.assertIn('"$(airc_rs_bin)" identity peer-pub', combined)
        self.assertIn('"$(airc_rs_bin)" envelope wrap', combined)
        self.assertIn('"$(airc_rs_bin)" gist get .kind', combined)

    def test_control_plane_helpers_use_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "airc",
                REPO / "lib/airc_bash/cmd_connect.sh",
                REPO / "lib/airc_bash/cmd_doctor.sh",
                REPO / "lib/airc_bash/cmd_identity.sh",
                REPO / "lib/airc_bash/cmd_status.sh",
            ]
        )

        forbidden = [
            "airc_core.gh_backoff run",
            "airc_core.gh_backoff wait-seconds",
            "airc_core.gh_backoff audit",
            "airc_core.gh_backoff doctor",
            "airc_core.pending_batch host-broadcast-route",
            "airc_core.handshake get_field",
        ]
        for needle in forbidden:
            self.assertNotIn(needle, combined)

        self.assertIn('"$(airc_rs_bin)" gh run', combined)
        self.assertIn('"$(airc_rs_bin)" gh wait-seconds', combined)
        self.assertIn('"$(airc_rs_bin)" gh doctor', combined)
        self.assertIn('"$(airc_rs_bin)" pending host-broadcast-route', combined)

    def test_message_json_glue_uses_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "lib/airc_bash/cmd_send.sh",
                REPO / "lib/airc_bash/cmd_connect.sh",
            ]
        )

        forbidden = [
            "json.dumps(sys.stdin.read())",
            "uuid.uuid4()",
            "HOST_X25519_PUB",
            "json.dump(record",
        ]
        for needle in forbidden:
            self.assertNotIn(needle, combined)

        self.assertIn('"$(airc_rs_bin)" message build-legacy', combined)
        self.assertIn('"$(airc_rs_bin)" uuid-v4', combined)
        self.assertIn('"$(airc_rs_bin)" config get --home "$AIRC_WRITE_DIR"', combined)
        self.assertIn('"$(airc_rs_bin)" identity write-peer-record', combined)

    def test_channel_gist_find_paths_use_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "lib/airc_bash/mesh.sh",
                REPO / "lib/airc_bash/cmd_connect.sh",
            ]
        )

        self.assertNotIn("airc_core.channel_gist find", combined)
        self.assertNotIn("airc_core.channel_gist host-preflight", combined)
        self.assertNotIn("airc_core.channel_gist resolve", combined)
        self.assertNotIn("airc_core.channel_gist remember-created", combined)
        self.assertIn('"$(airc_rs_bin)" channel-gist find', combined)
        self.assertIn('"$(airc_rs_bin)" channel-gist host-preflight', combined)
        self.assertIn('"$(airc_rs_bin)" channel-gist resolve', combined)
        self.assertIn('"$(airc_rs_bin)" channel-gist remember-created', combined)

    def test_bearer_send_paths_use_airc_rs(self):
        combined = "\n".join(
            path.read_text(encoding="utf-8")
            for path in [
                REPO / "airc",
                REPO / "lib/airc_bash/cmd_send.sh",
            ]
        )

        self.assertNotIn("airc_core.bearer_cli send ", combined)
        self.assertNotIn("airc_core.bearer_cli send-batch", combined)
        self.assertIn('"$(airc_rs_bin)" bearer send ', combined)
        self.assertIn('"$(airc_rs_bin)" bearer send-batch', combined)

    def test_bearer_recv_paths_use_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.bearer_cli recv", source)
        self.assertIn('"$(airc_rs_bin)" bearer recv "self"', source)
        self.assertIn('pkill -f "airc-rs bearer recv.*${AIRC_WRITE_DIR}"', source)

    def test_monitor_formatter_uses_airc_rs(self):
        source = (REPO / "airc").read_text(encoding="utf-8")
        start = source.index("monitor_formatter()")
        end = source.index("# cmd_connect extracted", start)
        body = source[start:end]

        self.assertNotIn("airc_core.monitor_formatter", body)
        self.assertIn('"$_airc_rs" monitor format', body)
        self.assertIn("airc-rs is required for monitor format", body)

    def test_attach_log_tail_uses_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_connect.sh").read_text(encoding="utf-8")
        start = source.index("_join_attach_local_stream()")
        end = source.index("_join_emit_join_events()", start)
        body = source[start:end]

        self.assertNotIn("airc_core.log_tail", body)
        self.assertIn('"$_airc_rs" --home "$AIRC_WRITE_DIR" monitor attach', body)
        self.assertIn("airc-rs is required for monitor attach", body)

    def test_handshake_runtime_paths_use_airc_rs(self):
        source = (REPO / "lib/airc_bash/cmd_connect.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.handshake send", source)
        self.assertNotIn("airc_core.handshake accept_one", source)
        self.assertIn('"$(airc_rs_bin)" handshake send', source)
        self.assertIn('"$(airc_rs_bin)" handshake accept-one', source)


if __name__ == "__main__":
    unittest.main()
