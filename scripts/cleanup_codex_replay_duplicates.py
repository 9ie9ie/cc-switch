#!/usr/bin/env python3
"""Remove legacy Codex subagent history-replay rows from CC Switch.

The old importer treated the parent-history snapshot at the start of a forked
Codex thread as fresh usage. Request ids are stable, so the replay prefix can be
derived from the source JSONL and removed without fuzzy token matching.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator


UUID_RE = re.compile(
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
)
LEGACY_PREFIX = "codex_session"
THREAD_V1_PREFIX = "codex_session:thread-v1"


@dataclass(frozen=True)
class ReplayPrefix:
    thread_id: str
    event_count: int
    source_path: Path
    reason: str
    legacy_ids_safe: bool = True


@dataclass
class Tokens:
    input: int = 0
    cached_input: int = 0
    output: int = 0
    reasoning_output: int = 0


def parse_args() -> argparse.Namespace:
    home = Path.home()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--db",
        type=Path,
        default=home / ".cc-switch" / "cc-switch.db",
        help="CC Switch SQLite database",
    )
    parser.add_argument(
        "--codex-dir",
        type=Path,
        default=home / ".codex",
        help="Codex configuration directory",
    )
    parser.add_argument(
        "--retain-days",
        type=int,
        default=30,
        help="keep this many local calendar days of Codex session usage",
    )
    parser.add_argument(
        "--backup-dir",
        type=Path,
        help="required with --apply; backup is written here before deletion",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="apply the analyzed deletion; default is read-only analysis",
    )
    return parser.parse_args()


def iter_codex_files(codex_dir: Path) -> Iterator[Path]:
    sessions = codex_dir / "sessions"
    if sessions.is_dir():
        yield from sessions.rglob("*.jsonl")

    archived = codex_dir / "archived_sessions"
    if archived.is_dir():
        yield from archived.glob("*.jsonl")


def parse_json(line: str) -> dict | None:
    try:
        value = json.loads(line)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None
    return value if isinstance(value, dict) else None


def session_meta(path: Path) -> dict | None:
    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if '"session_meta"' not in line:
                    continue
                value = parse_json(line)
                if value and value.get("type") == "session_meta":
                    payload = value.get("payload")
                    return payload if isinstance(payload, dict) else None
    except OSError:
        return None
    return None


def thread_identity(payload: dict) -> tuple[str | None, str | None, bool]:
    thread_id = next(
        (
            payload.get(key)
            for key in ("id", "thread_id", "threadId", "session_id", "sessionId")
            if isinstance(payload.get(key), str) and payload.get(key)
        ),
        None,
    )
    old_importer_id = next(
        (
            payload.get(key)
            for key in ("session_id", "sessionId", "id")
            if isinstance(payload.get(key), str) and payload.get(key)
        ),
        None,
    )
    source = payload.get("source")
    has_subagent_source = isinstance(source, dict) and "subagent" in source
    carries_snapshot = bool(payload.get("forked_from_id")) or has_subagent_source or (
        old_importer_id is not None
        and thread_id is not None
        and old_importer_id != thread_id
    )
    return thread_id, old_importer_id, carries_snapshot


def parse_tokens(usage: object) -> Tokens | None:
    if not isinstance(usage, dict):
        return None

    details = usage.get("output_tokens_details")
    completion_details = usage.get("completion_tokens_details")
    reasoning = usage.get("reasoning_output_tokens")
    if not isinstance(reasoning, int):
        reasoning = (
            details.get("reasoning_tokens", 0) if isinstance(details, dict) else 0
        )
    if not isinstance(reasoning, int):
        reasoning = (
            completion_details.get("reasoning_tokens", 0)
            if isinstance(completion_details, dict)
            else 0
        )

    def integer(key: str, fallback: str | None = None) -> int:
        value = usage.get(key)
        if not isinstance(value, int) and fallback:
            value = usage.get(fallback)
        return max(value, 0) if isinstance(value, int) else 0

    return Tokens(
        input=integer("input_tokens"),
        cached_input=integer("cached_input_tokens", "cache_read_input_tokens"),
        output=integer("output_tokens"),
        reasoning_output=max(reasoning, 0) if isinstance(reasoning, int) else 0,
    )


def token_delta(previous: Tokens | None, current: Tokens, is_total: bool) -> Tokens:
    if previous is None or not is_total:
        delta = current
    else:
        delta = Tokens(
            input=max(current.input - previous.input, 0),
            cached_input=max(current.cached_input - previous.cached_input, 0),
            output=max(current.output - previous.output, 0),
            reasoning_output=max(current.reasoning_output - previous.reasoning_output, 0),
        )
    delta.cached_input = min(delta.cached_input, delta.input)
    return delta


def is_boundary(value: dict) -> bool:
    event_type = value.get("type")
    if isinstance(event_type, str) and event_type.startswith(
        "inter_agent_communication"
    ):
        return True
    if event_type != "event_msg":
        return False
    payload = value.get("payload")
    return isinstance(payload, dict) and payload.get("type") == "thread_settings_applied"


def replay_event_count(path: Path) -> int | None:
    previous: Tokens | None = None
    event_count = 0

    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if (
                    '"token_count"' not in line
                    and '"thread_settings_applied"' not in line
                    and '"inter_agent_communication' not in line
                ):
                    continue
                value = parse_json(line)
                if not value:
                    continue
                if is_boundary(value):
                    return event_count
                if value.get("type") != "event_msg":
                    continue
                payload = value.get("payload")
                if not isinstance(payload, dict) or payload.get("type") != "token_count":
                    continue
                info = payload.get("info")
                if not isinstance(info, dict):
                    continue

                is_total = "total_token_usage" in info
                usage = info.get("total_token_usage") if is_total else info.get("last_token_usage")
                current = parse_tokens(usage)
                if current is None:
                    continue
                delta = token_delta(previous, current, is_total)
                if is_total:
                    previous = current
                if any(
                    (
                        delta.input,
                        delta.cached_input,
                        delta.output,
                        delta.reasoning_output,
                    )
                ):
                    event_count += 1
    except OSError:
        return None
    return None


def timestamp_cluster_replay_count(
    path: Path, session_timestamp: object, cluster_seconds: float = 5.0
) -> int | None:
    if not isinstance(session_timestamp, str):
        return None
    try:
        started_at = dt.datetime.fromisoformat(session_timestamp.replace("Z", "+00:00"))
    except ValueError:
        return None

    previous: Tokens | None = None
    event_count = 0
    saw_later_event = False

    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if '"timestamp"' not in line:
                    continue
                value = parse_json(line)
                if not value:
                    continue
                timestamp = value.get("timestamp")
                if isinstance(timestamp, str):
                    try:
                        event_time = dt.datetime.fromisoformat(
                            timestamp.replace("Z", "+00:00")
                        )
                    except ValueError:
                        event_time = None
                    if event_time and (event_time - started_at).total_seconds() > cluster_seconds:
                        saw_later_event = True
                        break

                if value.get("type") != "event_msg":
                    continue
                payload = value.get("payload")
                if not isinstance(payload, dict) or payload.get("type") != "token_count":
                    continue
                info = payload.get("info")
                if not isinstance(info, dict):
                    continue
                is_total = "total_token_usage" in info
                usage = (
                    info.get("total_token_usage")
                    if is_total
                    else info.get("last_token_usage")
                )
                current = parse_tokens(usage)
                if current is None:
                    continue
                delta = token_delta(previous, current, is_total)
                if is_total:
                    previous = current
                if any(
                    (
                        delta.input,
                        delta.cached_input,
                        delta.output,
                        delta.reasoning_output,
                    )
                ):
                    event_count += 1
    except OSError:
        return None

    # A fork containing only the copied history has no later event. Its full
    # token sequence is replay, provided at least one replay event was found.
    return event_count if event_count and (saw_later_event or event_count > 1) else None


def legacy_session_ids(conn: sqlite3.Connection) -> set[str]:
    rows = conn.execute(
        """
        SELECT DISTINCT session_id
        FROM proxy_request_logs
        WHERE data_source = 'codex_session'
          AND request_id LIKE 'codex_session:%'
          AND session_id IS NOT NULL
        """
    )
    return {row[0] for row in rows if isinstance(row[0], str)}


def legacy_usage_sequence(
    conn: sqlite3.Connection, session_id: str
) -> list[tuple[int, int, int, int]]:
    prefix = f"{LEGACY_PREFIX}:{session_id}:"
    return [
        tuple(int(value) for value in row)
        for row in conn.execute(
            """
            SELECT input_tokens, output_tokens, cache_read_tokens,
                   cache_creation_tokens
            FROM proxy_request_logs
            WHERE data_source = 'codex_session'
              AND request_id LIKE ?
            ORDER BY CAST(substr(request_id, ?) AS INTEGER)
            """,
            (f"{prefix}%", len(prefix) + 1),
        )
    ]


def common_parent_prefix_count(
    child: list[tuple[int, int, int, int]],
    parent: list[tuple[int, int, int, int]],
) -> int:
    count = 0
    for child_usage, parent_usage in zip(child, parent):
        if child_usage != parent_usage:
            break
        count += 1
    return count


def analyze_replay_prefixes(
    conn: sqlite3.Connection, codex_dir: Path, known_session_ids: set[str]
) -> tuple[list[ReplayPrefix], int, int, dict[str, int]]:
    prefixes: dict[str, ReplayPrefix] = {}
    sequence_cache: dict[str, list[tuple[int, int, int, int]]] = {}
    candidate_files = 0
    thread_v1_only_files = 0
    reason_counts = {"explicit": 0, "parent-prefix": 0, "timestamp-cluster": 0}

    for path in iter_codex_files(codex_dir):
        filename_id = UUID_RE.search(path.name)
        if filename_id and filename_id.group(0) not in known_session_ids:
            continue

        payload = session_meta(path)
        if not payload:
            continue
        thread_id, old_importer_id, carries_snapshot = thread_identity(payload)
        if not carries_snapshot or not thread_id or thread_id not in known_session_ids:
            continue
        candidate_files += 1

        # The old importer used session_id before id. If it differs from the
        # current thread id, legacy request ids can refer to parent history and
        # are unsafe. Current thread-v1 ids still identify the child safely.
        thread_v1_only = old_importer_id != thread_id
        if thread_v1_only:
            thread_v1_only_files += 1

        candidates: list[tuple[int, str]] = []
        explicit_count = replay_event_count(path)
        if explicit_count:
            candidates.append((explicit_count, "explicit"))

        parent_id = payload.get("forked_from_id")
        if isinstance(parent_id, str) and parent_id in known_session_ids:
            child_sequence = sequence_cache.setdefault(
                thread_id, legacy_usage_sequence(conn, thread_id)
            )
            parent_sequence = sequence_cache.setdefault(
                parent_id, legacy_usage_sequence(conn, parent_id)
            )
            parent_count = common_parent_prefix_count(child_sequence, parent_sequence)
            if parent_count:
                candidates.append((parent_count, "parent-prefix"))

        # Timestamp clustering remains a fallback for legacy-safe forks. A
        # current thread-v1 child can contain copied early markers, so for an id
        # mismatch compare the initial timestamp cluster with marker evidence
        # and retain the larger replay prefix.
        if thread_v1_only or not candidates:
            timestamp_count = timestamp_cluster_replay_count(
                path, payload.get("timestamp")
            )
            if timestamp_count:
                candidates.append((timestamp_count, "timestamp-cluster"))

        if candidates:
            count, reason = max(candidates, key=lambda item: item[0])
            current = prefixes.get(thread_id)
            legacy_ids_safe = not thread_v1_only and (
                current is None or current.legacy_ids_safe
            )
            if current is None or count > current.event_count:
                prefixes[thread_id] = ReplayPrefix(
                    thread_id, count, path, reason, legacy_ids_safe
                )
            elif current.legacy_ids_safe and not legacy_ids_safe:
                prefixes[thread_id] = ReplayPrefix(
                    current.thread_id,
                    current.event_count,
                    current.source_path,
                    current.reason,
                    False,
                )

    for prefix in prefixes.values():
        reason_counts[prefix.reason] += 1
    return list(prefixes.values()), candidate_files, thread_v1_only_files, reason_counts


def candidate_request_ids(prefixes: Iterable[ReplayPrefix]) -> set[str]:
    result: set[str] = set()
    for prefix in prefixes:
        for index in range(1, prefix.event_count + 1):
            result.add(f"{THREAD_V1_PREFIX}:{prefix.thread_id}:{index}")
            if prefix.legacy_ids_safe:
                result.add(f"{LEGACY_PREFIX}:{prefix.thread_id}:{index}")
    return result


def existing_candidate_stats(
    conn: sqlite3.Connection, request_ids: set[str]
) -> dict[str, int]:
    conn.execute("DROP TABLE IF EXISTS temp.codex_replay_cleanup_ids")
    conn.execute(
        "CREATE TEMP TABLE codex_replay_cleanup_ids (request_id TEXT PRIMARY KEY)"
    )
    conn.executemany(
        "INSERT OR IGNORE INTO codex_replay_cleanup_ids(request_id) VALUES (?)",
        ((request_id,) for request_id in request_ids),
    )
    # sqlite3 opens an implicit transaction for the TEMP-table inserts. Commit
    # it before the explicit cleanup transaction; TEMP rows remain available.
    conn.commit()
    stats_row = conn.execute(
        """
        SELECT COUNT(*),
               COALESCE(SUM(l.input_tokens), 0),
               COALESCE(SUM(l.output_tokens), 0),
               COALESCE(SUM(l.reasoning_output_tokens), 0),
               COALESCE(SUM(l.cache_read_tokens), 0)
        FROM proxy_request_logs l
        JOIN codex_replay_cleanup_ids c USING (request_id)
        WHERE l.data_source = 'codex_session'
        """
    ).fetchone()
    keys = ("rows", "input", "output", "reasoning", "cache_read")
    return dict(zip(keys, (int(value) for value in stats_row)))


def codex_totals(conn: sqlite3.Connection) -> dict[str, int]:
    row = conn.execute(
        """
        SELECT COUNT(*),
               COALESCE(SUM(input_tokens), 0),
               COALESCE(SUM(output_tokens), 0),
               COALESCE(SUM(reasoning_output_tokens), 0),
               COALESCE(SUM(cache_read_tokens), 0)
        FROM proxy_request_logs
        WHERE data_source = 'codex_session'
        """
    ).fetchone()
    keys = ("rows", "input", "output", "reasoning", "cache_read")
    return dict(zip(keys, (int(value) for value in row)))


def backup_database(conn: sqlite3.Connection, backup_dir: Path) -> Path:
    backup_dir.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_path = backup_dir / f"cc-switch-before-replay-dedup-{timestamp}.db"
    target = sqlite3.connect(backup_path)
    try:
        conn.backup(target)
    finally:
        target.close()
    return backup_path


def local_cutoff(retain_days: int) -> tuple[int, str]:
    if retain_days < 1:
        raise ValueError("retain-days must be at least 1")
    today = dt.datetime.now().astimezone().date()
    first_kept_date = today - dt.timedelta(days=retain_days - 1)
    midnight = dt.datetime.combine(
        first_kept_date, dt.time.min, tzinfo=dt.datetime.now().astimezone().tzinfo
    )
    return int(midnight.timestamp()), first_kept_date.isoformat()


def apply_cleanup(
    conn: sqlite3.Connection,
    cutoff_timestamp: int,
    cutoff_date: str,
) -> tuple[int, int, int]:
    conn.execute("BEGIN IMMEDIATE")
    try:
        deleted_replays = conn.execute(
            """
            DELETE FROM proxy_request_logs
            WHERE request_id IN (SELECT request_id FROM codex_replay_cleanup_ids)
              AND data_source = 'codex_session'
            """
        ).rowcount
        deleted_old_details = conn.execute(
            """
            DELETE FROM proxy_request_logs
            WHERE data_source = 'codex_session' AND created_at < ?
            """,
            (cutoff_timestamp,),
        ).rowcount
        deleted_old_rollups = conn.execute(
            """
            DELETE FROM usage_daily_rollups
            WHERE provider_id = '_codex_session' AND date < ?
            """,
            (cutoff_date,),
        ).rowcount
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    return deleted_replays, deleted_old_details, deleted_old_rollups


def main() -> int:
    args = parse_args()
    if args.apply and args.backup_dir is None:
        print("--backup-dir is required with --apply", file=sys.stderr)
        return 2
    if not args.db.is_file():
        print(f"database not found: {args.db}", file=sys.stderr)
        return 2
    if not args.codex_dir.is_dir():
        print(f"Codex directory not found: {args.codex_dir}", file=sys.stderr)
        return 2

    uri = f"file:{args.db.as_posix()}?mode={'rw' if args.apply else 'ro'}"
    conn = sqlite3.connect(uri, uri=True, timeout=30)
    try:
        before = codex_totals(conn)
        known_ids = legacy_session_ids(conn)
        prefixes, candidate_files, thread_v1_only_files, reason_counts = (
            analyze_replay_prefixes(conn, args.codex_dir, known_ids)
        )
        generated_ids = candidate_request_ids(prefixes)
        duplicate_stats = existing_candidate_stats(conn, generated_ids)
        cutoff_timestamp, cutoff_date = local_cutoff(args.retain_days)

        print(f"Codex detail rows before: {before['rows']:,}")
        print(f"Snapshot files found: {candidate_files:,}")
        print(f"Safe replay prefixes: {len(prefixes):,}")
        print(
            "Replay evidence: "
            f"explicit={reason_counts['explicit']:,}, "
            f"parent-prefix={reason_counts['parent-prefix']:,}, "
            f"timestamp-cluster={reason_counts['timestamp-cluster']:,}"
        )
        print(f"Thread-v1-only collision files: {thread_v1_only_files:,}")
        print(f"Duplicate rows matched: {duplicate_stats['rows']:,}")
        print(f"Duplicate input tokens: {duplicate_stats['input']:,}")
        print(f"Duplicate output tokens: {duplicate_stats['output']:,}")
        print(f"Duplicate reasoning tokens: {duplicate_stats['reasoning']:,}")
        print(f"Duplicate cache-read tokens: {duplicate_stats['cache_read']:,}")
        print(f"Retention starts at local date: {cutoff_date}")

        if not args.apply:
            print("Analysis only; no database changes were made.")
            return 0

        backup_path = backup_database(conn, args.backup_dir)
        deleted = apply_cleanup(conn, cutoff_timestamp, cutoff_date)
        after = codex_totals(conn)
        print(f"Backup: {backup_path}")
        print(f"Replay rows deleted: {deleted[0]:,}")
        print(f"Old detail rows deleted: {deleted[1]:,}")
        print(f"Old rollup rows deleted: {deleted[2]:,}")
        print(f"Codex detail rows after: {after['rows']:,}")
        print(f"Codex input tokens after: {after['input']:,}")
        return 0
    finally:
        conn.close()


if __name__ == "__main__":
    raise SystemExit(main())
