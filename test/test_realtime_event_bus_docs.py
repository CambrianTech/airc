"""Contract checks for the realtime event bus design."""

from __future__ import annotations

import pathlib
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = REPO_ROOT / "docs" / "realtime-event-bus.md"


class RealtimeEventBusDocTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.text = DOC.read_text(encoding="utf-8")

    def test_reuses_continuum_schema_families(self) -> None:
        for schema in (
            "JTAGRequest<T>",
            "JTAGMessage<T>",
            "EventBridgePayload",
            "GridFrame",
            "BridgeCommand",
            "BridgeEvent",
        ):
            self.assertIn(schema, self.text)

    def test_declares_airc_continuum_adapter_boundaries(self) -> None:
        for phrase in (
            "AIRC owns:",
            "Continuum owns:",
            "The adapter owns:",
            "AIRC should not define Continuum's domain packet hierarchy",
        ):
            self.assertIn(phrase, self.text)

    def test_traits_cover_subscription_presence_transport_and_schema(self) -> None:
        for trait_name in (
            "pub trait EventBus",
            "pub trait SubscriptionStore",
            "pub trait PresenceStore",
            "pub trait RealtimeTransport",
            "pub trait SchemaAdapter",
        ):
            self.assertIn(trait_name, self.text)

    def test_realtime_sql_and_media_boundary_are_explicit(self) -> None:
        for phrase in (
            "CREATE TABLE realtime_latest",
            "CREATE TABLE subscription_metrics",
            "CREATE TABLE request_response_index",
            "AIRC does not carry audio/video frames",
            "exclude_same_client",
        ):
            self.assertIn(phrase, self.text)

    def test_realtime_storage_is_internal_orm_not_consumer_sql(self) -> None:
        for phrase in (
            "ORM-backed SQLite indexes",
            "it must not query AIRC SQLite tables directly",
            "These tables are ORM migration targets inside AIRC",
            "not to invite application SQL",
        ):
            self.assertIn(phrase, self.text)


if __name__ == "__main__":
    unittest.main()
