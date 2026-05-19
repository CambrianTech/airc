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


if __name__ == "__main__":
    unittest.main()
