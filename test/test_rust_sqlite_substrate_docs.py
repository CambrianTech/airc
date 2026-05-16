"""Contract checks for the Rust SQLite substrate design."""

from __future__ import annotations

import pathlib
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = REPO_ROOT / "docs" / "rust-sqlite-substrate.md"


class RustSqliteSubstrateDocTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.text = DOC.read_text(encoding="utf-8")

    def test_declares_runtime_boundaries(self) -> None:
        for phrase in (
            "AIRC owns:",
            "Continuum owns:",
            "Adapters own:",
            "No adapter should own the state machine",
        ):
            self.assertIn(phrase, self.text)

    def test_schema_covers_required_runtime_state(self) -> None:
        for table in (
            "CREATE TABLE events",
            "CREATE TABLE subscriptions",
            "CREATE TABLE receipts",
            "CREATE TABLE outbox",
            "CREATE TABLE transport_cursors",
            "CREATE TABLE files",
            "CREATE TABLE queue_cards",
            "CREATE TABLE health_samples",
        ):
            self.assertIn(table, self.text)

    def test_traits_cover_store_projections_transport_and_blobs(self) -> None:
        for trait_name in (
            "pub trait EventStore",
            "pub trait Projection",
            "pub trait TransportAdapter",
            "pub trait BlobStore",
        ):
            self.assertIn(trait_name, self.text)

    def test_realtime_and_performance_requirements_are_explicit(self) -> None:
        for phrase in (
            "presence.typing",
            "presence.thinking",
            "webrtc.signaling",
            "livekit.control",
            "p50/p95/p99 append latency",
            "no polling loop above 1 percent CPU while idle",
        ):
            self.assertIn(phrase, self.text)


if __name__ == "__main__":
    unittest.main()
