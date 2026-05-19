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


if __name__ == "__main__":
    unittest.main()
