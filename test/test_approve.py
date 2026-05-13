"""Tests for `airc approve` + `airc decrypt-approval` — knock follow-up
verbs (airc#559 PR-2).

Coverage:
  - dispatch: both verbs reach the right cmd_ function + --help paths work
  - validation: malformed issue URLs / missing required args fail loudly
  - crypto: end-to-end roundtrip via the knock_crypto python helper
    (gen-knock-keys → encrypt-for-knocker → decrypt-from-approver)
  - envelope parsing: knocker_pub extracted from a knock issue body
  - tamper-resistance: ciphertext modification fails AEAD auth

The actual `gh issue comment` invocation is NOT exercised here (would
require a real GitHub issue + auth). cmd_approve's --dry-run path
exercises everything UP TO the gh call so the envelope shape is
verified end-to-end without external dependencies.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"
VENV_PYTHON = REPO_ROOT / ".venv" / "bin" / "python3"
LIB_DIR = REPO_ROOT / "lib"


def _python_with_crypto() -> str:
    """Find a python3 that has the cryptography package importable.

    Prefers the repo's venv if it exists (covers fresh-checkout test
    runs); falls back to system python3 (covers CI environments where
    cryptography is in the system site-packages).
    """
    if VENV_PYTHON.exists():
        return str(VENV_PYTHON)
    return "python3"


def run_airc(args: list[str], env_overrides: dict[str, str] | None = None
             ) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True, text=True, env=env,
        cwd=str(REPO_ROOT), timeout=15,
    )


def run_knock_crypto(args: list[str], stdin: str = "") -> subprocess.CompletedProcess[str]:
    """Invoke `python3 -m airc_core.knock_crypto <args>` directly.

    Used by the roundtrip test to drive the crypto layer without
    spinning up the full airc CLI.
    """
    env = os.environ.copy()
    env["PYTHONPATH"] = str(LIB_DIR) + os.pathsep + env.get("PYTHONPATH", "")
    return subprocess.run(
        [_python_with_crypto(), "-m", "airc_core.knock_crypto", *args],
        input=stdin, capture_output=True, text=True, env=env, timeout=15,
    )


class ApproveDispatchTests(unittest.TestCase):
    def test_approve_help_returns_zero(self) -> None:
        result = run_airc(["approve", "--help"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--invite", result.stdout)
        self.assertIn("airc-knock", result.stdout)

    def test_decrypt_approval_help_returns_zero(self) -> None:
        result = run_airc(["decrypt-approval", "--help"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--knocker-priv", result.stdout)


class ApproveValidationTests(unittest.TestCase):
    def test_approve_missing_url_fails(self) -> None:
        result = run_airc(["approve"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("issue-url", result.stdout + result.stderr)

    def test_approve_malformed_url_fails_with_clear_message(self) -> None:
        result = run_airc(["approve", "not-a-url"])
        self.assertNotEqual(result.returncode, 0)
        # cmd_approve calls `die` with a specific message; combined
        # output should contain the validation hint.
        combined = result.stdout + result.stderr
        self.assertIn("owner/repo", combined.lower())

    def test_decrypt_approval_missing_priv_fails(self) -> None:
        result = run_airc(
            ["decrypt-approval", "https://github.com/owner/repo/issues/1"]
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("knocker-priv", result.stdout + result.stderr)


class KnockCryptoRoundtripTests(unittest.TestCase):
    """End-to-end crypto roundtrip via the python helper.

    Skipped cleanly when cryptography isn't installed in the chosen
    python (fresh-checkout-no-venv CI case).
    """

    @classmethod
    def setUpClass(cls) -> None:
        result = run_knock_crypto(["gen-knock-keys"])
        if result.returncode != 0:
            raise unittest.SkipTest(
                f"knock_crypto unavailable (cryptography not installed?): "
                f"{result.stderr.strip()}"
            )

    def test_gen_knock_keys_emits_32_byte_hex_pair(self) -> None:
        result = run_knock_crypto(["gen-knock-keys"])
        self.assertEqual(result.returncode, 0, result.stderr)
        keys = json.loads(result.stdout)
        self.assertEqual(len(bytes.fromhex(keys["priv"])), 32)
        self.assertEqual(len(bytes.fromhex(keys["pub"])), 32)

    def test_full_roundtrip_recovers_plaintext(self) -> None:
        # 1. Knocker generates ephemeral keypair.
        keys = json.loads(run_knock_crypto(["gen-knock-keys"]).stdout)
        secret = "private-room://abc123#deadbeef"
        # 2. Approver encrypts to knocker_pub.
        approval = json.loads(run_knock_crypto([
            "encrypt-for-knocker",
            "--knocker-pub", keys["pub"],
            "--plaintext", secret,
        ]).stdout)
        self.assertEqual(approval["ver"], "v1")
        self.assertEqual(len(bytes.fromhex(approval["approver_pub"])), 32)
        self.assertEqual(len(bytes.fromhex(approval["nonce"])), 12)
        self.assertGreater(len(bytes.fromhex(approval["ciphertext"])), 16,
                           "ciphertext must include AEAD tag (>=16 bytes)")
        # 3. Knocker decrypts.
        decrypted = run_knock_crypto([
            "decrypt-from-approver",
            "--knocker-priv", keys["priv"],
            "--approver-pub", approval["approver_pub"],
            "--nonce", approval["nonce"],
            "--ciphertext", approval["ciphertext"],
        ])
        self.assertEqual(decrypted.returncode, 0, decrypted.stderr)
        self.assertEqual(decrypted.stdout.rstrip("\n"), secret)

    def test_tampered_ciphertext_fails_aead_auth(self) -> None:
        keys = json.loads(run_knock_crypto(["gen-knock-keys"]).stdout)
        approval = json.loads(run_knock_crypto([
            "encrypt-for-knocker",
            "--knocker-pub", keys["pub"],
            "--plaintext", "secret-room",
        ]).stdout)
        # Flip one bit in the ciphertext (XOR the last byte with 0x01).
        ct_bytes = bytearray(bytes.fromhex(approval["ciphertext"]))
        ct_bytes[-1] ^= 0x01
        tampered = ct_bytes.hex()
        result = run_knock_crypto([
            "decrypt-from-approver",
            "--knocker-priv", keys["priv"],
            "--approver-pub", approval["approver_pub"],
            "--nonce", approval["nonce"],
            "--ciphertext", tampered,
        ])
        # Decryption MUST fail loud — silent corruption would defeat the
        # whole point of AEAD.
        self.assertNotEqual(result.returncode, 0,
                            "tampered ciphertext must fail AEAD auth")
        self.assertIn("authentication failed", result.stderr.lower())

    def test_wrong_approver_pub_fails(self) -> None:
        # Two knockers; approver encrypts to A; B tries to decrypt with
        # A's approver_pub but B's own priv — must fail.
        keys_a = json.loads(run_knock_crypto(["gen-knock-keys"]).stdout)
        keys_b = json.loads(run_knock_crypto(["gen-knock-keys"]).stdout)
        approval = json.loads(run_knock_crypto([
            "encrypt-for-knocker",
            "--knocker-pub", keys_a["pub"],
            "--plaintext", "for-A-only",
        ]).stdout)
        result = run_knock_crypto([
            "decrypt-from-approver",
            "--knocker-priv", keys_b["priv"],  # wrong knocker!
            "--approver-pub", approval["approver_pub"],
            "--nonce", approval["nonce"],
            "--ciphertext", approval["ciphertext"],
        ])
        self.assertNotEqual(result.returncode, 0,
                            "wrong knocker priv must fail AEAD auth")


class KnockEnvelopeShapeTests(unittest.TestCase):
    """The knock issue body must carry the knocker_pub so cmd_approve
    can find it. Dry-run output is the canonical reference."""

    def test_knock_dry_run_embeds_knocker_pub_envelope(self) -> None:
        # Skip if crypto not available — knock falls back to empty pub.
        if run_knock_crypto(["gen-knock-keys"]).returncode != 0:
            self.skipTest("knock_crypto unavailable")
        with __import__('tempfile').TemporaryDirectory() as tmp:
            env = {
                "HOME": tmp,
                "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                "AIRC_NO_IDENTITY_PROMPT": "1",
                "PATH": "/usr/bin:/bin",
            }
            result = run_airc(
                ["knock", "owner/repo", "--dry-run", "-m", "test"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        # Find every fenced JSON block and look for one with knocker_pub.
        blocks = re.findall(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                            result.stdout, re.DOTALL)
        knocker_pub_seen = False
        for blob in blocks:
            try:
                parsed = json.loads(blob)
            except Exception:
                continue
            if isinstance(parsed, dict) and "knocker_pub" in parsed:
                knocker_pub_seen = True
                # Must be 32-byte hex (64 chars), or empty-string for
                # the no-crypto fallback path. We're in the crypto-OK
                # path here (gen-knock-keys succeeded in setUpClass).
                pub = parsed["knocker_pub"]
                self.assertEqual(len(pub), 64,
                                 f"knocker_pub should be 64-char hex; got {len(pub)}")
                break
        self.assertTrue(knocker_pub_seen,
                        f"knock dry-run must embed knocker_pub envelope; "
                        f"got blocks: {blocks}")


if __name__ == "__main__":
    unittest.main()
