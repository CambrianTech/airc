"""Bearer abstraction tests — the seam compiles, the ABC enforces its
contract, and the resolver picks correctly.

Run: python -m unittest test.test_bearer (from repo root)
or:  cd test && python -m unittest test_bearer
"""

from __future__ import annotations

import argparse
import sys
import unittest
from pathlib import Path

# Make lib/ importable when running this test from the repo root or test/.
REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from unittest import mock  # noqa: E402

from airc_core.bearer import (  # noqa: E402
    Bearer,
    BearerError,
    LivenessResult,
    PeerUnreachable,
    ReceivedMessage,
    SendOutcome,
)
from airc_core.bearer_resolver import (  # noqa: E402
    available_kinds,
    resolve,
)
from airc_core.bearer_ssh import SshBearer, SshBearerError  # noqa: E402
from airc_core.bearer_local import LocalBearer, LocalBearerError  # noqa: E402
from airc_core.bearer_gh import GhBearer, GhBearerError  # noqa: E402
from airc_core import bearer_ssh  # noqa: E402
from airc_core import bearer_local  # noqa: E402
from airc_core import bearer_gh  # noqa: E402
from airc_core import bearer_cli  # noqa: E402


class BearerInterfaceTests(unittest.TestCase):
    """The ABC enforces its shape and refuses incomplete implementations."""

    def test_cannot_instantiate_abstract_bearer(self):
        with self.assertRaises(TypeError):
            Bearer()

    def test_partial_subclass_cannot_instantiate(self):
        # Missing required abstract methods → instantiation refused.
        class HalfBearer(Bearer):
            KIND = "half"

            @classmethod
            def can_serve(cls, peer_meta):
                return False

            # Deliberately missing: open, send, recv_stream, liveness, close.

        with self.assertRaises(TypeError):
            HalfBearer()

    def test_received_message_is_immutable(self):
        m = ReceivedMessage(
            sender_peer_id="alice",
            channel="general",
            payload=b"hello",
        )
        with self.assertRaises(Exception):
            m.payload = b"tampered"  # frozen dataclass

    def test_liveness_result_allows_unknown_last_seen(self):
        r = LivenessResult(peer_id="bob", last_seen_ts=None, bearer_diag="no signal")
        self.assertIsNone(r.last_seen_ts)


class ResolverTests(unittest.TestCase):
    """Resolver picks bearers based on can_serve, raises when no candidate."""

    def test_available_kinds_includes_ssh_in_phase0(self):
        kinds = available_kinds()
        self.assertIn("ssh", kinds)

    def test_resolves_ssh_for_peer_with_host_target(self):
        bearer = resolve({"host_target": "user@host:7547"})
        self.assertIsInstance(bearer, SshBearer)
        self.assertEqual(bearer.KIND, "ssh")
        bearer.close()

    def test_unreachable_when_no_bearer_can_serve(self):
        with self.assertRaises(PeerUnreachable):
            resolve({})  # no host_target, no bearer matches

    def test_resolved_bearer_is_not_yet_open(self):
        bearer = resolve({"host_target": "user@host:7547"})
        # Resolution is cheap — no IO happens yet.
        self.assertIsInstance(bearer, Bearer)
        bearer.close()

    def test_resolver_passes_peer_meta_to_bearer(self):
        # Bearer needs peer_meta to send; resolver must thread it through
        # at construction.
        bearer = resolve({"host_target": "alice@example:7547"})
        self.assertEqual(bearer._peer_meta.get("host_target"), "alice@example:7547")
        bearer.close()


class SshBearerSkeletonTests(unittest.TestCase):
    """Phase 0 SshBearer skeleton: lifecycle methods, NotImplementedError
    guidance for the parts that arrive in Phase 1+."""

    def test_kind_is_ssh(self):
        self.assertEqual(SshBearer.KIND, "ssh")

    def test_can_serve_requires_host_target(self):
        self.assertTrue(SshBearer.can_serve({"host_target": "u@h"}))
        self.assertFalse(SshBearer.can_serve({}))
        self.assertFalse(SshBearer.can_serve({"unrelated": "field"}))

    def test_can_serve_is_pure(self):
        # No IO, no side effects — calling it 100 times is free.
        for _ in range(100):
            SshBearer.can_serve({"host_target": "u@h"})

    def test_construct_is_cheap(self):
        # Cheap-construct invariant: resolver may build candidates
        # speculatively. No IO on __init__.
        b1 = SshBearer()
        b2 = SshBearer({"host_target": "u@h"})
        self.assertIsNot(b1, b2)
        b1.close()
        b2.close()

    def test_open_then_close_is_clean(self):
        b = SshBearer({"host_target": "u@h"})
        b.open("alice")
        b.close()
        # close() is idempotent
        b.close()

    def test_post_close_operations_raise(self):
        b = SshBearer({"host_target": "u@h"})
        b.close()
        with self.assertRaises(BearerError):
            b.open("alice")
        with self.assertRaises(BearerError):
            b.send("alice", "general", b"x")


class SshBearerSendTests(unittest.TestCase):
    """Phase 1 SshBearer.send() — the relocated SSH delivery primitive.

    All tests mock subprocess.run + the tailscale resolver so no real
    network or processes are touched. We verify the bearer correctly
    classifies outcomes from the underlying transport's signals.
    """

    def setUp(self):
        # Default peer_meta — overridden per test as needed.
        self._meta = {
            "host_target": "alice@example:7547",
            "remote_home": "$HOME/.airc",
            "identity_key": "/tmp/fake_key",
        }

    def _bearer(self, meta=None):
        b = SshBearer(meta or self._meta)
        b.open("alice")
        return b

    def test_send_without_host_target_raises(self):
        b = SshBearer({})  # no host_target
        b.open("alice")
        with self.assertRaises(SshBearerError) as ctx:
            b.send("alice", "general", b"hi")
        self.assertIn("host_target", str(ctx.exception))
        b.close()

    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=True)
    def test_send_queues_when_tailnet_reports_offline(self, _mock_offline):
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "queued_unreachable")
        self.assertIn("offline", outcome.detail.lower())
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_delivered_when_marker_in_stdout(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"__APPENDED__\n",
            stderr=b"",
            returncode=0,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "delivered")
        # Verify the SSH invocation was constructed correctly.
        args = mock_run.call_args
        argv = args.args[0]
        self.assertIn("/usr/bin/ssh", argv)
        self.assertIn("-i", argv)
        self.assertIn("/tmp/fake_key", argv)
        self.assertIn("-p", argv)
        self.assertIn("7547", argv)
        # Remote command must contain the messages.jsonl append + marker.
        self.assertTrue(any("messages.jsonl" in a for a in argv))
        self.assertTrue(any("__APPENDED__" in a for a in argv))
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_classifies_auth_failure(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"",
            stderr=b"alice@example: Permission denied (publickey).\n",
            returncode=255,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "auth_failure")
        self.assertIn("re-pair", outcome.detail)
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_classifies_transient_failure(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"",
            stderr=b"ssh: connect to host example port 7547: Connection refused\n",
            returncode=255,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("Connection refused", outcome.detail)
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(
        bearer_ssh.subprocess,
        "run",
        side_effect=bearer_ssh.subprocess.TimeoutExpired(cmd="ssh", timeout=15),
    )
    def test_send_handles_timeout(self, *_):
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("timed out", outcome.detail)
        b.close()

    def test_send_outcome_is_immutable(self):
        o = SendOutcome(kind="delivered")
        with self.assertRaises(Exception):
            o.kind = "tampered"


class SshBearerRecvStreamTests(unittest.TestCase):
    """Phase 2a SshBearer.recv_stream() — yields ReceivedMessage events
    parsed from ssh tail's stdout. Tests mock subprocess.Popen so no real
    network is touched.
    """

    def _bearer(self, meta=None):
        m = meta or {
            "host_target": "alice@example:7547",
            "remote_home": "$HOME/.airc",
            "identity_key": "/tmp/fake_key",
        }
        b = SshBearer(m)
        b.open("alice")
        return b

    def _fake_proc(self, lines, returncode=0):
        """Build a mock subprocess that yields `lines` via readline() then EOF.

        The bearer reads via while-loop + readline() so EOF (empty bytes)
        terminates the inner loop. Each line in `lines` is returned in order;
        after exhaustion, readline returns b'' to signal EOF.
        """
        proc = mock.Mock()
        line_iter = iter(list(lines) + [b""])  # b"" signals EOF
        proc.stdout = mock.Mock()
        proc.stdout.readline = mock.Mock(side_effect=lambda: next(line_iter))
        proc.poll = mock.Mock(return_value=returncode)
        proc.terminate = mock.Mock()
        proc.wait = mock.Mock(return_value=returncode)
        proc.kill = mock.Mock()
        return proc

    def test_recv_stream_without_host_target_raises(self):
        b = SshBearer({})
        b.open("alice")
        with self.assertRaises(SshBearerError):
            next(b.recv_stream())
        b.close()

    def test_envelope_parser_drops_non_json(self):
        # Junk line → None
        self.assertIsNone(SshBearer._parse_envelope(b"not json\n"))
        # Empty line → None
        self.assertIsNone(SshBearer._parse_envelope(b"\n"))
        # JSON but not an object → None
        self.assertIsNone(SshBearer._parse_envelope(b"[1,2,3]\n"))
        # Object missing `from` → None
        self.assertIsNone(SshBearer._parse_envelope(b'{"channel":"x"}\n'))

    def test_envelope_parser_accepts_well_formed(self):
        line = b'{"from":"bob","channel":"general","msg":"hi"}\n'
        msg = SshBearer._parse_envelope(line)
        self.assertIsNotNone(msg)
        self.assertEqual(msg.sender_peer_id, "bob")
        self.assertEqual(msg.channel, "general")
        # Payload is the original line (sans trailing newline)
        self.assertEqual(msg.payload, b'{"from":"bob","channel":"general","msg":"hi"}')
        self.assertIn("envelope", msg.bearer_metadata)
        self.assertEqual(msg.bearer_metadata["envelope"]["msg"], "hi")

    def test_compute_tail_position_no_offset_file(self):
        self.assertEqual(SshBearer._compute_tail_position(None), "-n 0")

    def test_compute_tail_position_invalid_offsets(self):
        import tempfile
        for content in ("", "0", "abc", "-1", "  "):
            with tempfile.NamedTemporaryFile("w", delete=False) as f:
                f.write(content)
                path = f.name
            self.assertEqual(
                SshBearer._compute_tail_position(path), "-n 0",
                f"content={content!r} should produce -n 0",
            )

    def test_compute_tail_position_resumes_past_saved_line(self):
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("42")
            path = f.name
        self.assertEqual(SshBearer._compute_tail_position(path), "-n +43")

    @mock.patch.object(bearer_ssh.subprocess, "Popen")
    def test_recv_stream_yields_parsed_envelopes(self, mock_popen):
        lines = [
            b'{"from":"bob","channel":"general","msg":"hello"}\n',
            b'{"from":"carol","channel":"general","msg":"world"}\n',
            b'corrupted line\n',  # should be silently dropped
            b'{"from":"dave","channel":"useideem","msg":"hi"}\n',
        ]
        mock_popen.return_value = self._fake_proc(lines)

        b = self._bearer()
        events = []
        # Take 3 events then close (stops the iterator). The mock proc's
        # stdout iterator will exhaust naturally.
        gen = b.recv_stream()
        for ev in gen:
            events.append(ev)
            if len(events) >= 3:
                b.close()
                break

        self.assertEqual(len(events), 3)
        self.assertEqual(events[0].sender_peer_id, "bob")
        self.assertEqual(events[1].sender_peer_id, "carol")
        # Note: events[2] is "dave" — the malformed line was skipped.
        self.assertEqual(events[2].sender_peer_id, "dave")

    @mock.patch.object(bearer_ssh.subprocess, "Popen")
    def test_liveness_updates_on_each_event(self, mock_popen):
        lines = [b'{"from":"bob","channel":"general","msg":"hi"}\n']
        mock_popen.return_value = self._fake_proc(lines)

        b = self._bearer()
        # Pre-stream: no signal
        live_before = b.liveness("alice")
        self.assertIsNone(live_before.last_seen_ts)
        self.assertIn("no events", live_before.bearer_diag.lower())

        # Consume one event, check liveness BEFORE closing
        gen = b.recv_stream()
        next(gen)
        live_after = b.liveness("alice")
        self.assertIsNotNone(live_after.last_seen_ts)
        self.assertIn("ssh tail", live_after.bearer_diag.lower())
        b.close()

    @mock.patch.object(bearer_ssh.subprocess, "Popen")
    def test_recv_stream_persists_offset(self, mock_popen):
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("0")
            offset_path = f.name

        lines = [
            b'{"from":"bob","channel":"general","msg":"a"}\n',
            b'{"from":"bob","channel":"general","msg":"b"}\n',
        ]
        mock_popen.return_value = self._fake_proc(lines)

        meta = {
            "host_target": "alice@example:7547",
            "remote_home": "$HOME/.airc",
            "identity_key": "/tmp/fake_key",
            "offset_file": offset_path,
        }
        b = self._bearer(meta)

        gen = b.recv_stream()
        next(gen)
        next(gen)
        b.close()

        with open(offset_path) as f:
            self.assertEqual(f.read().strip(), "2")

    def test_close_terminates_recv_subprocess(self):
        b = self._bearer()
        # Simulate a running proc
        fake_proc = mock.Mock()
        fake_proc.poll = mock.Mock(return_value=None)  # still running
        fake_proc.terminate = mock.Mock()
        fake_proc.wait = mock.Mock(return_value=0)
        fake_proc.kill = mock.Mock()
        b._proc = fake_proc

        b.close()
        fake_proc.terminate.assert_called_once()


class CgnatRegexTests(unittest.TestCase):
    """The Tailscale-CGNAT range matcher is the only Tailscale knowledge
    in the codebase outside install scripts. Until Phase 3 deletes it,
    it must reject non-CGNAT IPs cleanly so no LAN/DNS targets get
    mis-routed through the offline-fast-path."""

    def test_matches_cgnat_addresses(self):
        for ip in ("100.64.0.1", "100.99.99.99", "100.119.50.20", "100.127.255.254"):
            self.assertTrue(bearer_ssh._CGNAT_RE.match(ip), f"should match {ip}")

    def test_rejects_non_cgnat_addresses(self):
        for ip in ("100.63.0.1", "100.128.0.1", "192.168.1.1", "10.0.0.1", "127.0.0.1", "100.5.0.1"):
            self.assertFalse(bearer_ssh._CGNAT_RE.match(ip), f"should reject {ip}")

    def test_offline_check_strips_user_prefix(self):
        # Strips user@ correctly so resume paths with `user@host` form
        # don't bypass the CGNAT gate. (issue #78 root cause)
        with mock.patch.object(bearer_ssh, "_resolve_tailscale_bin", return_value=None):
            # No tailscale = always False. Just verify no crash on user@host form.
            self.assertFalse(bearer_ssh._is_peer_offline_in_tailnet("alice@100.64.0.1"))
            self.assertFalse(bearer_ssh._is_peer_offline_in_tailnet("alice@192.168.1.5"))


class BearerCliRecvTests(unittest.TestCase):
    """Phase 2b: `python -m airc_core.bearer_cli recv` is the bridge from
    bash monitor → bearer.recv_stream(). The CLI must:
      - Print one line per envelope (raw payload bytes + \\n)
      - Pass offset_file through to the bearer for reconnect resume
      - Exit cleanly on resolver error (with stderr explanation)
      - Exit cleanly on BrokenPipeError (formatter died)
    Tests substitute a fake bearer for the resolver to keep them hermetic.
    """

    class _FakeBearer:
        """Records open()/close()/recv_stream() calls; yields fixed events."""
        def __init__(self, peer_meta):
            self.peer_meta = peer_meta
            self.opened_for = None
            self.closed = False
            self._events = []

        def set_events(self, events):
            self._events = events

        def open(self, peer_id):
            self.opened_for = peer_id

        def recv_stream(self):
            for ev in self._events:
                yield ev

        def close(self):
            self.closed = True

    def _make_args(self, **overrides):
        ns = argparse.Namespace(
            peer_id="alice",
            host_target="alice@example",
            identity_key="/tmp/k",
            remote_home="$HOME/.airc",
            offset_file=None,
            state_file=None,
        )
        for k, v in overrides.items():
            setattr(ns, k, v)
        return ns

    def _capture_stdout_bytes(self):
        """Replace sys.stdout with one whose .buffer captures bytes.

        cmd_recv writes to sys.stdout.buffer (binary). Our capture
        intercepts at that level and lets us read what the CLI emitted.
        """
        import io
        captured = io.BytesIO()
        fake_stdout = mock.Mock()
        fake_stdout.buffer = captured
        return fake_stdout, captured

    def test_recv_emits_one_line_per_envelope(self):
        events = [
            ReceivedMessage(
                sender_peer_id="bob",
                channel="general",
                payload=b'{"from":"bob","channel":"general","msg":"hi"}',
                bearer_metadata={},
            ),
            ReceivedMessage(
                sender_peer_id="carol",
                channel="general",
                payload=b'{"from":"carol","channel":"general","msg":"hey"}\n',
                bearer_metadata={},
            ),
        ]
        fake = self._FakeBearer({})
        fake.set_events(events)

        fake_stdout, captured = self._capture_stdout_bytes()
        with mock.patch.object(bearer_cli, "resolve", return_value=fake), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            rc = bearer_cli.cmd_recv(self._make_args())

        self.assertEqual(rc, 0)
        out_lines = captured.getvalue().splitlines(keepends=True)
        self.assertEqual(len(out_lines), 2)
        # First payload had no newline; CLI must add one.
        self.assertEqual(
            out_lines[0],
            b'{"from":"bob","channel":"general","msg":"hi"}\n',
        )
        # Second payload already had a trailing newline; CLI must not double it.
        self.assertEqual(
            out_lines[1],
            b'{"from":"carol","channel":"general","msg":"hey"}\n',
        )
        self.assertTrue(fake.closed, "bearer.close() must be called")
        self.assertEqual(fake.opened_for, "alice")

    def test_recv_passes_offset_file_to_resolver(self):
        captured_meta = {}

        def fake_resolve(meta):
            captured_meta.update(meta)
            fake = self._FakeBearer(meta)
            return fake

        fake_stdout, _ = self._capture_stdout_bytes()
        with mock.patch.object(bearer_cli, "resolve", side_effect=fake_resolve), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            bearer_cli.cmd_recv(self._make_args(offset_file="/tmp/monitor_offset"))

        self.assertEqual(captured_meta.get("offset_file"), "/tmp/monitor_offset")

    def test_recv_drops_none_meta_fields(self):
        captured_meta = {}

        def fake_resolve(meta):
            captured_meta.update(meta)
            return self._FakeBearer(meta)

        fake_stdout, _ = self._capture_stdout_bytes()
        with mock.patch.object(bearer_cli, "resolve", side_effect=fake_resolve), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            bearer_cli.cmd_recv(self._make_args(
                identity_key=None, offset_file=None,
            ))

        self.assertNotIn("identity_key", captured_meta)
        self.assertNotIn("offset_file", captured_meta)
        self.assertEqual(captured_meta.get("host_target"), "alice@example")

    def test_recv_resolver_error_returns_2(self):
        fake_stderr = mock.Mock()

        def fake_resolve(meta):
            raise RuntimeError("no bearer can serve this peer")

        with mock.patch.object(bearer_cli, "resolve", side_effect=fake_resolve), \
             mock.patch.object(bearer_cli.sys, "stderr", fake_stderr):
            rc = bearer_cli.cmd_recv(self._make_args())

        self.assertEqual(rc, 2)
        # The error must be surfaced (CLAUDE.md: never swallow errors).
        printed = "".join(
            call.args[0] if call.args else ""
            for call in fake_stderr.print.call_args_list
        ) if hasattr(fake_stderr, "print") else ""
        # `print(file=sys.stderr)` calls .write on the file. Inspect that path.
        write_calls = [c.args[0] for c in fake_stderr.write.call_args_list]
        joined = "".join(str(x) for x in write_calls)
        self.assertIn("resolver error", joined)

    def test_recv_broken_pipe_exits_cleanly(self):
        events = [
            ReceivedMessage(
                sender_peer_id="bob",
                channel="general",
                payload=b'{"from":"bob","channel":"general","msg":"first"}',
                bearer_metadata={},
            ),
            ReceivedMessage(
                sender_peer_id="bob",
                channel="general",
                payload=b'{"from":"bob","channel":"general","msg":"second"}',
                bearer_metadata={},
            ),
        ]
        fake = self._FakeBearer({})
        fake.set_events(events)

        # Buffer that raises BrokenPipeError on the second write — simulates
        # the formatter exiting after consuming one line.
        class _BrokenAfter:
            def __init__(self, n):
                self.n = n
                self.writes = 0
                self.flushes = 0

            def write(self, _data):
                self.writes += 1
                if self.writes > self.n:
                    raise BrokenPipeError()

            def flush(self):
                self.flushes += 1

        broken = _BrokenAfter(n=1)
        fake_stdout = mock.Mock()
        fake_stdout.buffer = broken

        with mock.patch.object(bearer_cli, "resolve", return_value=fake), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            rc = bearer_cli.cmd_recv(self._make_args())

        self.assertEqual(rc, 0, "broken pipe must produce graceful exit")
        self.assertTrue(fake.closed, "bearer.close() must run on broken pipe")

    def test_parser_recognizes_recv(self):
        # Smoke-test: the recv subparser must be registered with the
        # right defaults so a typo in cmd binding doesn't slip past.
        parser = bearer_cli._build_parser()
        ns = parser.parse_args([
            "recv", "alice",
            "--host-target", "alice@example",
            "--identity-key", "/tmp/k",
            "--remote-home", "$HOME/.airc",
            "--offset-file", "/tmp/off",
            "--state-file", "/tmp/state",
        ])
        self.assertEqual(ns.cmd, "recv")
        self.assertEqual(ns.peer_id, "alice")
        self.assertEqual(ns.host_target, "alice@example")
        self.assertEqual(ns.identity_key, "/tmp/k")
        self.assertEqual(ns.remote_home, "$HOME/.airc")
        self.assertEqual(ns.offset_file, "/tmp/off")
        self.assertEqual(ns.state_file, "/tmp/state")
        self.assertIs(ns.func, bearer_cli.cmd_recv)


class BearerCliStateFileTests(unittest.TestCase):
    """Phase 2c: bearer_cli recv writes a per-event state file that
    `airc status` reads. The state file is the bearer-attested liveness
    surface that replaces the messages.jsonl-mirror lie identified in
    #270 (status said 'fresh' while bearer was actually wedged).
    Test contract:
      - On launch, state file is initialized with last_recv_ts=None.
      - Each event rewrites it with monotonically growing events_total.
      - The write is atomic (no half-written file on race).
      - kind/peer_id/diag passthrough from the bearer is preserved.
    """

    class _FakeBearer:
        KIND = "fake-ssh"

        def __init__(self, peer_meta):
            self.peer_meta = peer_meta
            self._events = []
            self._next_ts = 1714435200.0
            self.closed = False

        def set_events(self, events):
            self._events = events

        def open(self, peer_id):
            self._peer_id = peer_id

        def recv_stream(self):
            for ev in self._events:
                yield ev

        def liveness(self, peer_id):
            self._next_ts += 1.0
            return LivenessResult(
                peer_id=peer_id,
                last_seen_ts=self._next_ts,
                bearer_diag="last event from fake bearer",
            )

        def close(self):
            self.closed = True

    def _make_args(self, state_file):
        return argparse.Namespace(
            peer_id="alice",
            host_target="alice@example",
            identity_key="/tmp/k",
            remote_home="$HOME/.airc",
            offset_file=None,
            state_file=state_file,
        )

    def _capture_stdout_bytes(self):
        import io
        captured = io.BytesIO()
        fake_stdout = mock.Mock()
        fake_stdout.buffer = captured
        return fake_stdout, captured

    def test_state_file_initialized_on_launch(self):
        import json as _json
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".json") as f:
            state_path = f.name

        fake = self._FakeBearer({})
        # No events; the bearer closes after the (empty) iter.
        fake.set_events([])

        fake_stdout, _ = self._capture_stdout_bytes()
        with mock.patch.object(bearer_cli, "resolve", return_value=fake), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            bearer_cli.cmd_recv(self._make_args(state_path))

        with open(state_path) as f:
            state = _json.load(f)
        self.assertEqual(state["kind"], "fake-ssh")
        self.assertEqual(state["peer_id"], "alice")
        self.assertIsNone(state["last_recv_ts"])
        self.assertEqual(state["events_total"], 0)
        self.assertIn("no events yet", state["diag"].lower())

    def test_state_file_updated_per_event(self):
        import json as _json
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".json") as f:
            state_path = f.name

        events = [
            ReceivedMessage(
                sender_peer_id="bob",
                channel="general",
                payload=b'{"from":"bob","channel":"general","msg":"hi"}',
                bearer_metadata={},
            ),
            ReceivedMessage(
                sender_peer_id="carol",
                channel="general",
                payload=b'{"from":"carol","channel":"general","msg":"hey"}',
                bearer_metadata={},
            ),
        ]
        fake = self._FakeBearer({})
        fake.set_events(events)

        fake_stdout, _ = self._capture_stdout_bytes()
        with mock.patch.object(bearer_cli, "resolve", return_value=fake), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout):
            bearer_cli.cmd_recv(self._make_args(state_path))

        with open(state_path) as f:
            state = _json.load(f)
        self.assertEqual(state["events_total"], 2)
        self.assertEqual(state["last_sender"], "carol")
        self.assertIsNotNone(state["last_recv_ts"])
        # Bearer's liveness ts was 1714435200 + (2 events × 1s for liveness call) + 1 (init) → check it's reasonable
        self.assertGreater(state["last_recv_ts"], 1714435200.0)
        self.assertEqual(state["kind"], "fake-ssh")

    def test_no_state_file_means_no_writes(self):
        events = [ReceivedMessage(
            sender_peer_id="bob",
            channel="general",
            payload=b'{"from":"bob","msg":"x"}',
            bearer_metadata={},
        )]
        fake = self._FakeBearer({})
        fake.set_events(events)

        fake_stdout, captured = self._capture_stdout_bytes()
        # Patch _write_state_file to detect any unwanted call.
        with mock.patch.object(bearer_cli, "resolve", return_value=fake), \
             mock.patch.object(bearer_cli.sys, "stdout", fake_stdout), \
             mock.patch.object(bearer_cli, "_write_state_file") as mock_write:
            bearer_cli.cmd_recv(self._make_args(state_file=None))

        mock_write.assert_not_called()
        # But events still flow to stdout — state-file is purely additive.
        self.assertIn(b'{"from":"bob","msg":"x"}', captured.getvalue())

    def test_state_file_write_is_atomic_via_replace(self):
        """The write must use os.replace (or equivalent) so a reader
        never sees a half-written file. We assert that no .json file
        with broken JSON ever appears at the target path during write.
        """
        import json as _json
        import tempfile
        import os as _os
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".json") as f:
            f.write('{"old": "state"}')
            state_path = f.name

        # _write_state_file should leave the target either unchanged or
        # fully rewritten — never empty/partial.
        bearer_cli._write_state_file(state_path, {
            "kind": "ssh",
            "peer_id": "alice",
            "last_recv_ts": 12345.0,
            "last_sender": "bob",
            "events_total": 5,
            "diag": "ok",
        })

        with open(state_path) as f:
            state = _json.load(f)  # must not raise
        self.assertEqual(state["events_total"], 5)
        _os.unlink(state_path)


class LocalBearerCanServeTests(unittest.TestCase):
    """Phase 3a: LocalBearer.can_serve must be conservative — only True
    when host_target is a literal loopback alias AND remote_home is a
    writable local directory. Both conditions must hold; either alone is
    a footgun (stale loopback record without dir, or unrelated local
    dir whose path collides with a remote scope name)."""

    def setUp(self):
        import tempfile
        self._tmpdir = tempfile.mkdtemp(prefix="airc-test-localbearer-")

    def tearDown(self):
        import shutil
        shutil.rmtree(self._tmpdir, ignore_errors=True)

    def test_serves_loopback_with_writable_dir(self):
        for ht in ("127.0.0.1", "localhost", "::1", "[::1]",
                   "user@127.0.0.1", "user@localhost",
                   "user@127.0.0.1:7547", "127.0.0.1:7547",
                   "user@[::1]:7547"):
            self.assertTrue(
                LocalBearer.can_serve({"host_target": ht, "remote_home": self._tmpdir}),
                f"should serve loopback host_target={ht!r}",
            )

    def test_rejects_non_loopback(self):
        for ht in ("alice@example.com", "user@192.168.1.5",
                   "user@100.91.51.87", "100.64.0.1",
                   "user@10.0.0.5"):
            self.assertFalse(
                LocalBearer.can_serve({"host_target": ht, "remote_home": self._tmpdir}),
                f"should NOT serve non-loopback host_target={ht!r}",
            )

    def test_rejects_when_remote_home_missing(self):
        self.assertFalse(
            LocalBearer.can_serve({"host_target": "127.0.0.1"}),
            "should reject when remote_home is absent",
        )
        self.assertFalse(
            LocalBearer.can_serve({"host_target": "127.0.0.1", "remote_home": ""}),
            "should reject when remote_home is empty string",
        )

    def test_rejects_when_remote_home_does_not_exist(self):
        bogus = self._tmpdir + "/does-not-exist"
        self.assertFalse(
            LocalBearer.can_serve({"host_target": "127.0.0.1", "remote_home": bogus}),
            "should reject when remote_home points at missing dir",
        )

    def test_rejects_when_remote_home_is_a_file(self):
        import os
        path = os.path.join(self._tmpdir, "not-a-dir.txt")
        with open(path, "w") as f:
            f.write("hi")
        self.assertFalse(
            LocalBearer.can_serve({"host_target": "127.0.0.1", "remote_home": path}),
            "should reject when remote_home points at a file (not dir)",
        )

    def test_can_serve_is_pure(self):
        # No IO outside of the os.access call. Repeat invocations don't
        # mutate peer_meta.
        meta = {"host_target": "127.0.0.1", "remote_home": self._tmpdir}
        before = dict(meta)
        for _ in range(3):
            LocalBearer.can_serve(meta)
        self.assertEqual(meta, before)


class LocalBearerSendTests(unittest.TestCase):
    """LocalBearer.send appends to remote_home/messages.jsonl directly."""

    def setUp(self):
        import tempfile
        self._tmpdir = tempfile.mkdtemp(prefix="airc-test-local-send-")
        self._bearer = LocalBearer({
            "host_target": "127.0.0.1",
            "remote_home": self._tmpdir,
        })
        self._bearer.open("alice")

    def tearDown(self):
        import shutil
        self._bearer.close()
        shutil.rmtree(self._tmpdir, ignore_errors=True)

    def test_send_appends_with_trailing_newline(self):
        outcome = self._bearer.send("alice", "general", b'{"from":"bob","msg":"hi"}')
        self.assertEqual(outcome.kind, "delivered")
        import os
        path = os.path.join(self._tmpdir, "messages.jsonl")
        with open(path, "rb") as f:
            content = f.read()
        self.assertEqual(content, b'{"from":"bob","msg":"hi"}\n')

    def test_send_preserves_existing_trailing_newline(self):
        self._bearer.send("alice", "general", b'{"x":1}\n')
        import os
        with open(os.path.join(self._tmpdir, "messages.jsonl"), "rb") as f:
            content = f.read()
        self.assertEqual(content, b'{"x":1}\n', "must not double-newline")

    def test_send_appends_does_not_truncate(self):
        self._bearer.send("alice", "general", b'{"a":1}')
        self._bearer.send("alice", "general", b'{"b":2}')
        import os
        with open(os.path.join(self._tmpdir, "messages.jsonl"), "rb") as f:
            content = f.read()
        self.assertEqual(content, b'{"a":1}\n{"b":2}\n')

    def test_send_reports_transient_when_dir_vanishes(self):
        # Race: directory disappears between can_serve and send.
        import shutil
        shutil.rmtree(self._tmpdir, ignore_errors=True)
        outcome = self._bearer.send("alice", "general", b'{"x":1}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("local append failed", outcome.detail)
        # Re-create for tearDown to clean up cleanly.
        import os
        os.makedirs(self._tmpdir, exist_ok=True)


class LocalBearerRecvTests(unittest.TestCase):
    """LocalBearer.recv_stream tails remote_home/messages.jsonl with
    pure-Python poll-based reads. Tests the file format contract; the
    integration scenario covers real-time tail behavior under load."""

    def setUp(self):
        import tempfile
        self._tmpdir = tempfile.mkdtemp(prefix="airc-test-local-recv-")
        self._meta = {
            "host_target": "127.0.0.1",
            "remote_home": self._tmpdir,
        }

    def tearDown(self):
        import shutil
        shutil.rmtree(self._tmpdir, ignore_errors=True)

    def _write(self, lines):
        import os
        with open(os.path.join(self._tmpdir, "messages.jsonl"), "ab") as f:
            for ln in lines:
                f.write(ln if ln.endswith(b"\n") else ln + b"\n")

    def test_recv_yields_pre_existing_lines_when_no_offset(self):
        # No offset_file → start position is 0 (read from beginning).
        # That matches "skip 0 lines" in _compute_skip_lines.
        self._write([
            b'{"from":"bob","channel":"general","msg":"first"}',
            b'{"from":"carol","channel":"general","msg":"second"}',
        ])
        b = LocalBearer(self._meta)
        b.open("alice")
        events = []
        gen = b.recv_stream()
        for ev in gen:
            events.append(ev)
            if len(events) >= 2:
                b.close()
                break
        self.assertEqual(len(events), 2)
        self.assertEqual(events[0].sender_peer_id, "bob")
        self.assertEqual(events[1].sender_peer_id, "carol")

    def test_recv_resumes_past_offset_file(self):
        import os, tempfile
        self._write([
            b'{"from":"bob","msg":"a"}',
            b'{"from":"bob","msg":"b"}',
            b'{"from":"bob","msg":"c"}',
        ])
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("2")  # skip first 2 lines
            offset_path = f.name
        try:
            meta = dict(self._meta, offset_file=offset_path)
            b = LocalBearer(meta)
            b.open("alice")
            events = []
            gen = b.recv_stream()
            for ev in gen:
                events.append(ev)
                if len(events) >= 1:
                    b.close()
                    break
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0].bearer_metadata["envelope"]["msg"], "c")
        finally:
            os.unlink(offset_path)

    def test_recv_drops_malformed_lines_silently(self):
        self._write([
            b'not json',
            b'{"from":"bob","msg":"good"}',
            b'[1,2,3]',  # JSON but not an object
            b'{"from":"carol","msg":"also good"}',
        ])
        b = LocalBearer(self._meta)
        b.open("alice")
        events = []
        gen = b.recv_stream()
        for ev in gen:
            events.append(ev)
            if len(events) >= 2:
                b.close()
                break
        self.assertEqual([e.sender_peer_id for e in events], ["bob", "carol"])

    def test_liveness_updates_on_each_event(self):
        self._write([b'{"from":"bob","msg":"x"}'])
        b = LocalBearer(self._meta)
        b.open("alice")
        live_before = b.liveness("alice")
        self.assertIsNone(live_before.last_seen_ts)
        gen = b.recv_stream()
        next(gen)
        live_after = b.liveness("alice")
        self.assertIsNotNone(live_after.last_seen_ts)
        self.assertIn("local tail", live_after.bearer_diag.lower())
        b.close()

    def test_offset_persists_after_recv(self):
        import os, tempfile
        self._write([
            b'{"from":"bob","msg":"a"}',
            b'{"from":"bob","msg":"b"}',
        ])
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("0")
            offset_path = f.name
        try:
            meta = dict(self._meta, offset_file=offset_path)
            b = LocalBearer(meta)
            b.open("alice")
            gen = b.recv_stream()
            next(gen); next(gen)
            b.close()
            with open(offset_path) as f:
                self.assertEqual(f.read().strip(), "2")
        finally:
            os.unlink(offset_path)


class LocalBearerSkeletonTests(unittest.TestCase):
    """Bearer ABC contract — same shape as SshBearerSkeletonTests."""

    def test_kind_is_local(self):
        self.assertEqual(LocalBearer.KIND, "local")

    def test_construct_is_cheap(self):
        # Constructor must not touch IO. Pass a peer_meta with a
        # nonexistent dir; construction succeeds, no exception.
        b = LocalBearer({"host_target": "127.0.0.1", "remote_home": "/nope/nope"})
        # Closing without open is a no-op (idempotent).
        b.close()

    def test_post_close_operations_raise(self):
        b = LocalBearer({"host_target": "127.0.0.1", "remote_home": "/tmp"})
        b.open("alice")
        b.close()
        with self.assertRaises(LocalBearerError):
            b.send("alice", "general", b'{"x":1}')


class ResolverOrderTests(unittest.TestCase):
    """Phase 3a: LocalBearer must be picked over SshBearer when both
    can serve a peer. SshBearer is the universal fallback; LocalBearer
    is the same-machine optimization. Reversing this order would make
    every same-machine 2-tab session waste SSH crypto cycles."""

    def setUp(self):
        import tempfile
        self._tmpdir = tempfile.mkdtemp(prefix="airc-test-resolver-")

    def tearDown(self):
        import shutil
        shutil.rmtree(self._tmpdir, ignore_errors=True)

    def test_local_first_for_same_machine_peer(self):
        bearer = resolve({
            "host_target": "user@127.0.0.1",
            "remote_home": self._tmpdir,
        })
        self.assertEqual(bearer.KIND, "local",
                         "same-machine peer must resolve to LocalBearer")

    def test_ssh_for_remote_peer(self):
        bearer = resolve({
            "host_target": "alice@example.com",
            "remote_home": "/home/alice/.airc",
        })
        self.assertEqual(bearer.KIND, "ssh",
                         "remote peer must resolve to SshBearer")

    def test_ssh_when_loopback_target_but_no_local_dir(self):
        # Stale loopback record without a host_airc_home that exists →
        # LocalBearer.can_serve is False → fall through to SshBearer.
        bearer = resolve({
            "host_target": "user@127.0.0.1",
            "remote_home": "/this/path/definitely/does/not/exist",
        })
        self.assertEqual(bearer.KIND, "ssh")

    def test_available_kinds_includes_local(self):
        from airc_core.bearer_resolver import available_kinds
        kinds = available_kinds()
        self.assertIn("local", kinds)
        self.assertIn("ssh", kinds)
        # local must come first (preference order).
        self.assertLess(kinds.index("local"), kinds.index("ssh"))


class GhBearerCanServeTests(unittest.TestCase):
    """Phase 3b: GhBearer.can_serve must require BOTH room_gist_id
    AND a working gh auth. Either alone is insufficient — gist id with
    no auth means we can't actually read/write; auth with no gist id
    means we have nowhere to send to."""

    def test_serves_with_gist_id_and_gh_auth(self):
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=True):
            self.assertTrue(GhBearer.can_serve({"room_gist_id": "abc123"}))

    def test_rejects_without_gist_id(self):
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=True):
            self.assertFalse(GhBearer.can_serve({}))
            self.assertFalse(GhBearer.can_serve({"room_gist_id": ""}))

    def test_rejects_without_gh_auth(self):
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=False):
            self.assertFalse(GhBearer.can_serve({"room_gist_id": "abc123"}))

    def test_can_serve_does_not_mutate_meta(self):
        meta = {"room_gist_id": "abc123"}
        before = dict(meta)
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=True):
            GhBearer.can_serve(meta)
        self.assertEqual(meta, before)


class GhBearerSendTests(unittest.TestCase):
    """GhBearer.send: read-modify-write of the room gist's messages.jsonl
    file. Tests mock _gh_api_get (the read step) and _gh_gist_write_file
    (the write step) so no real gh API is touched."""

    def _bearer(self, meta=None):
        m = meta or {"room_gist_id": "abc123"}
        b = GhBearer(m)
        b.open("alice")
        return b

    def test_send_appends_to_existing_messages_file(self):
        existing = '{"from":"x","msg":"old"}\n'
        captured = {}

        def fake_write(gist_id, content):
            captured["gist_id"] = gist_id
            captured["content"] = content
            return (True, "")

        with mock.patch.object(bearer_gh, "_gh_api_get",
                               return_value={"files": {"messages.jsonl": {"content": existing}}}), \
             mock.patch.object(bearer_gh, "_gh_gist_write_file", side_effect=fake_write):
            outcome = self._bearer().send("alice", "general", b'{"from":"bob","msg":"hi"}')

        self.assertEqual(outcome.kind, "delivered")
        self.assertEqual(captured["gist_id"], "abc123")
        self.assertEqual(
            captured["content"],
            existing + '{"from":"bob","msg":"hi"}\n',
        )

    def test_send_creates_messages_file_when_absent(self):
        # First write to the gist — messages.jsonl doesn't exist yet.
        captured = {}

        def fake_write(gist_id, content):
            captured["content"] = content
            return (True, "")

        with mock.patch.object(bearer_gh, "_gh_api_get",
                               return_value={"files": {}}), \
             mock.patch.object(bearer_gh, "_gh_gist_write_file", side_effect=fake_write):
            outcome = self._bearer().send("alice", "general", b'{"from":"bob","msg":"first"}')

        self.assertEqual(outcome.kind, "delivered")
        self.assertEqual(captured["content"], '{"from":"bob","msg":"first"}\n')

    def test_send_preserves_existing_trailing_newline(self):
        captured = {}

        def fake_write(gist_id, content):
            captured["content"] = content
            return (True, "")

        with mock.patch.object(bearer_gh, "_gh_api_get",
                               return_value={"files": {}}), \
             mock.patch.object(bearer_gh, "_gh_gist_write_file", side_effect=fake_write):
            self._bearer().send("alice", "general", b'{"x":1}\n')

        self.assertEqual(captured["content"], '{"x":1}\n')

    def test_send_transient_when_get_fails(self):
        with mock.patch.object(bearer_gh, "_gh_api_get", return_value=None):
            outcome = self._bearer().send("alice", "general", b'{"x":1}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("could not fetch gist", outcome.detail)

    def test_send_transient_when_write_fails(self):
        with mock.patch.object(bearer_gh, "_gh_api_get",
                               return_value={"files": {}}), \
             mock.patch.object(bearer_gh, "_gh_gist_write_file",
                               return_value=(False, "Network is unreachable")):
            outcome = self._bearer().send("alice", "general", b'{"x":1}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("Network is unreachable", outcome.detail)

    def test_send_auth_failure_on_permission_denied(self):
        with mock.patch.object(bearer_gh, "_gh_api_get",
                               return_value={"files": {}}), \
             mock.patch.object(bearer_gh, "_gh_gist_write_file",
                               return_value=(False, "HTTP 401: Permission denied")):
            outcome = self._bearer().send("alice", "general", b'{"x":1}')
        self.assertEqual(outcome.kind, "auth_failure")

    def test_send_without_gist_id_raises(self):
        b = GhBearer({})
        b.open("alice")
        with self.assertRaises(GhBearerError):
            b.send("alice", "general", b'{"x":1}')


class GhBearerRecvTests(unittest.TestCase):
    """GhBearer.recv_stream: poll the gist, yield new lines.

    Tests use poll_interval=0 in peer_meta so the loop doesn't sleep
    between iterations — keeps tests fast. Real production uses the
    15s default."""

    def _bearer(self, meta=None):
        m = meta or {"room_gist_id": "abc123", "poll_interval": 0}
        b = GhBearer(m)
        b.open("alice")
        return b

    def _gist_response(self, content):
        return {"files": {"messages.jsonl": {"content": content}}}

    def test_recv_yields_new_lines_per_poll(self):
        # First poll sees 2 lines; second poll sees 3 (1 new). Bearer
        # must yield only the new line on the second poll.
        responses = [
            self._gist_response(
                '{"from":"bob","channel":"general","msg":"a"}\n'
                '{"from":"carol","channel":"general","msg":"b"}\n'
            ),
            self._gist_response(
                '{"from":"bob","channel":"general","msg":"a"}\n'
                '{"from":"carol","channel":"general","msg":"b"}\n'
                '{"from":"dave","channel":"general","msg":"c"}\n'
            ),
        ]

        b = self._bearer()
        with mock.patch.object(bearer_gh, "_gh_api_get", side_effect=responses):
            events = []
            gen = b.recv_stream()
            # Take the first 3 events: 2 from poll1, 1 from poll2.
            for ev in gen:
                events.append(ev)
                if len(events) >= 3:
                    b.close()
                    break

        msgs = [ev.bearer_metadata["envelope"]["msg"] for ev in events]
        self.assertEqual(msgs, ["a", "b", "c"])

    def test_recv_skips_malformed_lines(self):
        with mock.patch.object(
            bearer_gh, "_gh_api_get",
            return_value=self._gist_response(
                'not json\n'
                '{"from":"bob","msg":"good"}\n'
                '[1,2,3]\n'
                '{"from":"carol","msg":"also good"}\n'
            ),
        ):
            b = self._bearer()
            events = []
            gen = b.recv_stream()
            for ev in gen:
                events.append(ev)
                if len(events) >= 2:
                    b.close()
                    break

        self.assertEqual([e.sender_peer_id for e in events], ["bob", "carol"])

    def test_recv_handles_get_failures_by_polling_again(self):
        # First poll fails (None); second succeeds. Bearer should sleep
        # and re-poll without crashing.
        responses = [
            None,  # transient
            self._gist_response('{"from":"bob","msg":"hi"}\n'),
        ]
        with mock.patch.object(bearer_gh, "_gh_api_get", side_effect=responses):
            b = self._bearer()
            events = []
            gen = b.recv_stream()
            for ev in gen:
                events.append(ev)
                b.close()
                break

        self.assertEqual(len(events), 1)
        self.assertEqual(events[0].sender_peer_id, "bob")

    def test_recv_resumes_past_offset_file(self):
        import tempfile, os as _os
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("2")  # skip first 2 lines
            offset_path = f.name
        try:
            with mock.patch.object(
                bearer_gh, "_gh_api_get",
                return_value=self._gist_response(
                    '{"from":"bob","msg":"a"}\n'
                    '{"from":"bob","msg":"b"}\n'
                    '{"from":"bob","msg":"c"}\n'
                ),
            ):
                b = self._bearer({
                    "room_gist_id": "abc123",
                    "poll_interval": 0,
                    "offset_file": offset_path,
                })
                events = []
                gen = b.recv_stream()
                for ev in gen:
                    events.append(ev)
                    b.close()
                    break

            self.assertEqual(len(events), 1)
            self.assertEqual(events[0].bearer_metadata["envelope"]["msg"], "c")
        finally:
            _os.unlink(offset_path)

    def test_liveness_updates_on_each_event(self):
        with mock.patch.object(
            bearer_gh, "_gh_api_get",
            return_value=self._gist_response('{"from":"bob","msg":"x"}\n'),
        ):
            b = self._bearer()
            live_before = b.liveness("alice")
            self.assertIsNone(live_before.last_seen_ts)
            self.assertIn("no events", live_before.bearer_diag.lower())

            gen = b.recv_stream()
            next(gen)
            live_after = b.liveness("alice")
            self.assertIsNotNone(live_after.last_seen_ts)
            self.assertIn("gh poll", live_after.bearer_diag.lower())
            b.close()

    def test_offset_persists_after_recv(self):
        import tempfile, os as _os
        with tempfile.NamedTemporaryFile("w", delete=False) as f:
            f.write("0")
            offset_path = f.name
        try:
            with mock.patch.object(
                bearer_gh, "_gh_api_get",
                return_value=self._gist_response(
                    '{"from":"bob","msg":"a"}\n'
                    '{"from":"bob","msg":"b"}\n'
                ),
            ):
                b = self._bearer({
                    "room_gist_id": "abc123",
                    "poll_interval": 0,
                    "offset_file": offset_path,
                })
                gen = b.recv_stream()
                next(gen); next(gen)
                b.close()

            with open(offset_path) as f:
                self.assertEqual(f.read().strip(), "2")
        finally:
            _os.unlink(offset_path)


class GhBearerSkeletonTests(unittest.TestCase):
    """ABC contract — same shape as SshBearerSkeleton/LocalBearerSkeleton."""

    def test_kind_is_gh(self):
        self.assertEqual(GhBearer.KIND, "gh")

    def test_construct_is_cheap(self):
        # Constructor must do NO IO. _has_gh_auth() must NOT run here —
        # it's only invoked by can_serve. We verify by patching it to
        # raise; if construction touched it, this would fail.
        with mock.patch.object(bearer_gh, "_has_gh_auth",
                               side_effect=AssertionError("must not run on construct")):
            b = GhBearer({"room_gist_id": "abc"})
            b.close()

    def test_post_close_operations_raise(self):
        b = GhBearer({"room_gist_id": "abc"})
        b.open("alice")
        b.close()
        with self.assertRaises(GhBearerError):
            b.send("alice", "general", b'{"x":1}')


class ResolverIncludesGhBearerTests(unittest.TestCase):
    """Phase 3b: GhBearer is registered (after SshBearer in 3b's
    additive ordering). Production peer_meta with host_target still
    routes to SshBearer; only meta lacking host_target reaches gh."""

    def test_available_kinds_includes_gh(self):
        from airc_core.bearer_resolver import available_kinds
        kinds = available_kinds()
        self.assertIn("gh", kinds)
        # 3b: gh comes AFTER ssh so today's traffic isn't preempted.
        self.assertGreater(kinds.index("gh"), kinds.index("ssh"))

    def test_ssh_still_wins_when_host_target_present(self):
        # Today's production peer_meta. Resolver picks SshBearer; gh
        # never gets a turn.
        bearer = resolve({
            "host_target": "alice@example.com",
            "remote_home": "/home/alice/.airc",
        })
        self.assertEqual(bearer.KIND, "ssh")

    def test_gh_picked_when_only_room_gist_id(self):
        # Phase 3b activation path. peer_meta has NO host_target
        # (so SSH and Local both decline) but has a room_gist_id and
        # gh auth works → GhBearer.
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=True):
            bearer = resolve({"room_gist_id": "abc123"})
        self.assertEqual(bearer.KIND, "gh")

    def test_unreachable_when_no_gh_auth_and_no_other_meta(self):
        # Nothing can serve: no host_target (Ssh declines), no loopback
        # (Local declines), no gh auth (Gh declines).
        with mock.patch.object(bearer_gh, "_has_gh_auth", return_value=False):
            with self.assertRaises(PeerUnreachable):
                resolve({"room_gist_id": "abc123"})


if __name__ == "__main__":
    unittest.main()
