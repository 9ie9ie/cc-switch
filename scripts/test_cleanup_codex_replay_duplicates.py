import datetime as dt
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

import cleanup_codex_replay_duplicates as cleanup


STARTED_AT = dt.datetime(2026, 7, 18, 1, 2, 3, tzinfo=dt.timezone.utc)
PARENT_ID = "11111111-1111-4111-8111-111111111111"
CHILD_ID = "22222222-2222-4222-8222-222222222222"
SAFE_ID = "33333333-3333-4333-8333-333333333333"


def iso_timestamp(offset_seconds: float) -> str:
    value = STARTED_AT + dt.timedelta(seconds=offset_seconds)
    return value.isoformat().replace("+00:00", "Z")


def token_event(event_index: int, offset_seconds: float) -> dict:
    return {
        "timestamp": iso_timestamp(offset_seconds),
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": event_index,
                    "cached_input_tokens": 0,
                    "output_tokens": 0,
                }
            },
        },
    }


def thread_settings_marker(offset_seconds: float) -> dict:
    return {
        "timestamp": iso_timestamp(offset_seconds),
        "type": "event_msg",
        "payload": {"type": "thread_settings_applied"},
    }


def copied_inter_agent_marker(offset_seconds: float) -> dict:
    return {
        "timestamp": iso_timestamp(offset_seconds),
        "type": "inter_agent_communication copied",
        "payload": {},
    }


class CleanupCodexReplayDuplicatesTests(unittest.TestCase):
    def write_session(
        self,
        codex_dir: Path,
        thread_id: str,
        old_importer_id: str,
        replay_events: int,
        first_marker_after: int,
    ) -> Path:
        sessions_dir = codex_dir / "sessions"
        sessions_dir.mkdir(parents=True)
        path = sessions_dir / f"rollout-{thread_id}.jsonl"
        records = [
            {
                "timestamp": iso_timestamp(0),
                "type": "session_meta",
                "payload": {
                    "id": thread_id,
                    "thread_id": thread_id,
                    "session_id": old_importer_id,
                    "timestamp": iso_timestamp(0),
                    "source": {"subagent": {"parent_thread_id": PARENT_ID}},
                },
            }
        ]

        for event_index in range(1, replay_events + 1):
            records.append(token_event(event_index, event_index / 1000))
            if event_index == first_marker_after:
                records.append(thread_settings_marker((event_index + 0.1) / 1000))
            if event_index == replay_events // 2:
                records.append(copied_inter_agent_marker((event_index + 0.2) / 1000))

        records.append(thread_settings_marker(2.0))
        records.append(token_event(replay_events + 1, 10.0))
        with path.open("w", encoding="utf-8", newline="\n") as handle:
            for record in records:
                handle.write(json.dumps(record, separators=(",", ":")) + "\n")
        return path

    def test_mismatch_uses_1476_event_cluster_and_only_child_thread_v1_ids(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            codex_dir = Path(temp_dir)
            path = self.write_session(
                codex_dir,
                thread_id=CHILD_ID,
                old_importer_id=PARENT_ID,
                replay_events=1476,
                first_marker_after=16,
            )

            self.assertEqual(cleanup.replay_event_count(path), 16)
            self.assertEqual(
                cleanup.timestamp_cluster_replay_count(path, iso_timestamp(0)),
                1476,
            )
            with sqlite3.connect(":memory:") as conn:
                prefixes, candidate_files, thread_v1_only_files, reasons = (
                    cleanup.analyze_replay_prefixes(
                        conn, codex_dir, {PARENT_ID, CHILD_ID}
                    )
                )

            self.assertEqual(candidate_files, 1)
            self.assertEqual(thread_v1_only_files, 1)
            self.assertEqual(reasons["timestamp-cluster"], 1)
            self.assertEqual(len(prefixes), 1)
            self.assertEqual(prefixes[0].event_count, 1476)
            self.assertEqual(prefixes[0].reason, "timestamp-cluster")
            self.assertFalse(prefixes[0].legacy_ids_safe)

            request_ids = cleanup.candidate_request_ids(prefixes)
            expected_prefix = f"{cleanup.THREAD_V1_PREFIX}:{CHILD_ID}:"
            self.assertEqual(len(request_ids), 1476)
            self.assertTrue(all(value.startswith(expected_prefix) for value in request_ids))
            self.assertIn(f"{expected_prefix}1", request_ids)
            self.assertIn(f"{expected_prefix}1476", request_ids)
            self.assertFalse(
                any(
                    value.startswith(f"{cleanup.LEGACY_PREFIX}:{thread_id}:")
                    for thread_id in (PARENT_ID, CHILD_ID)
                    for value in request_ids
                )
            )

    def test_legacy_safe_file_keeps_first_boundary_precedence(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            codex_dir = Path(temp_dir)
            path = self.write_session(
                codex_dir,
                thread_id=SAFE_ID,
                old_importer_id=SAFE_ID,
                replay_events=7,
                first_marker_after=3,
            )

            self.assertEqual(cleanup.replay_event_count(path), 3)
            self.assertEqual(
                cleanup.timestamp_cluster_replay_count(path, iso_timestamp(0)), 7
            )
            with sqlite3.connect(":memory:") as conn:
                prefixes, candidate_files, thread_v1_only_files, reasons = (
                    cleanup.analyze_replay_prefixes(conn, codex_dir, {SAFE_ID})
                )

            self.assertEqual(candidate_files, 1)
            self.assertEqual(thread_v1_only_files, 0)
            self.assertEqual(reasons["explicit"], 1)
            self.assertEqual(len(prefixes), 1)
            self.assertEqual(prefixes[0].event_count, 3)
            self.assertEqual(prefixes[0].reason, "explicit")
            self.assertTrue(prefixes[0].legacy_ids_safe)
            self.assertEqual(
                cleanup.candidate_request_ids(prefixes),
                {
                    f"{prefix}:{SAFE_ID}:{event_index}"
                    for prefix in (cleanup.LEGACY_PREFIX, cleanup.THREAD_V1_PREFIX)
                    for event_index in range(1, 4)
                },
            )


if __name__ == "__main__":
    unittest.main()
