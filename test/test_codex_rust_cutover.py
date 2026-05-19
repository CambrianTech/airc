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
        source = (REPO / "lib/airc_bash/cmd_doctor.sh").read_text(encoding="utf-8")

        self.assertNotIn("airc_core.bearer_state", source)
        self.assertIn('"$(airc_rs_bin)" bearer-state "$state_file"', source)

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


if __name__ == "__main__":
    unittest.main()
