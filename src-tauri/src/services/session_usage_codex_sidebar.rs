//! Import prospective Codex Desktop side-conversation usage captured by the
//! local stdio bridge. Historical ephemeral conversations are intentionally
//! not estimated because their request-level usage is not persisted by Codex.

use crate::config::get_app_config_dir;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{
    get_sync_state, metadata_modified_nanos, update_sync_state_on_conn, SessionSyncResult,
};
use crate::services::sql_helpers::INPUT_TOKEN_SEMANTICS_TOTAL;
use crate::services::usage_stats::{
    find_model_pricing, merge_reasoning_into_matching_proxy_log, should_skip_session_insert,
    DedupKey, SESSION_PROXY_DEDUP_WINDOW_SECONDS,
};
use rusqlite::OptionalExtension;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CODEX_SIDEBAR_USAGE_PATH_ENV: &str = "CC_SWITCH_CODEX_SIDEBAR_USAGE_PATH";
pub const CODEX_SIDEBAR_USAGE_FILE: &str = "codex-sidebar-usage.jsonl";
const EVENT_ID_PREFIX: &str = "codex_sidebar:usage-v1:";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidebarUsageEvent {
    schema_version: u32,
    event_id: String,
    thread_id: String,
    turn_id: String,
    model: String,
    completed_at_ms: i64,
    usage: SidebarTokenUsage,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidebarTokenUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    #[allow(dead_code)]
    total_tokens: u64,
}

impl SidebarUsageEvent {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!("unsupported schemaVersion {}", self.schema_version));
        }
        if !self.event_id.starts_with(EVENT_ID_PREFIX) {
            return Err("invalid eventId namespace".to_string());
        }
        if self.thread_id.trim().is_empty() || self.turn_id.trim().is_empty() {
            return Err("threadId and turnId are required".to_string());
        }
        Ok(())
    }

    fn token_usage(&self) -> Result<TokenUsage, String> {
        let input_tokens = to_u32("inputTokens", self.usage.input_tokens)?;
        let output_tokens = to_u32("outputTokens", self.usage.output_tokens)?;
        let reasoning_output_tokens =
            to_u32("reasoningOutputTokens", self.usage.reasoning_output_tokens)?;
        let cache_read_tokens = to_u32("cachedInputTokens", self.usage.cached_input_tokens)?;
        let cache_creation_tokens =
            to_u32("cacheWriteInputTokens", self.usage.cache_write_input_tokens)?;

        Ok(TokenUsage {
            input_tokens,
            output_tokens,
            reasoning_output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            model: Some(self.model.clone()),
            message_id: None,
        })
    }
}

fn to_u32(field: &str, value: u64) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{field} exceeds u32"))
}

pub fn sidebar_usage_path() -> PathBuf {
    std::env::var_os(CODEX_SIDEBAR_USAGE_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| get_app_config_dir().join(CODEX_SIDEBAR_USAGE_FILE))
}

pub fn sync_codex_sidebar_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    sync_codex_sidebar_usage_file(db, &sidebar_usage_path())
}

fn sync_codex_sidebar_usage_file(
    db: &Database,
    path: &Path,
) -> Result<SessionSyncResult, AppError> {
    if !path.is_file() {
        return Ok(SessionSyncResult::default());
    }

    let metadata = fs::metadata(path).map_err(|error| {
        AppError::Config(format!(
            "read Codex sidebar usage metadata failed {}: {error}",
            path.display()
        ))
    })?;
    let modified = metadata_modified_nanos(&metadata);
    let file_len = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    let path_key = path.to_string_lossy().to_string();
    let (last_modified, saved_offset) = get_sync_state(db, &path_key)?;

    if modified == last_modified && saved_offset == file_len {
        return Ok(SessionSyncResult {
            files_scanned: 1,
            ..SessionSyncResult::default()
        });
    }

    let start_offset = if saved_offset >= 0 && saved_offset <= file_len {
        saved_offset
    } else {
        0
    };
    let mut file = File::open(path).map_err(|error| {
        AppError::Config(format!(
            "open Codex sidebar usage failed {}: {error}",
            path.display()
        ))
    })?;
    file.seek(SeekFrom::Start(start_offset as u64))
        .map_err(|error| {
            AppError::Config(format!(
                "seek Codex sidebar usage failed {}: {error}",
                path.display()
            ))
        })?;

    let mut reader = BufReader::new(file);
    let mut result = SessionSyncResult {
        files_scanned: 1,
        ..SessionSyncResult::default()
    };
    let mut events = Vec::new();
    let mut processed_offset = start_offset;

    loop {
        let mut bytes = Vec::new();
        let read = reader.read_until(b'\n', &mut bytes).map_err(|error| {
            AppError::Config(format!(
                "read Codex sidebar usage failed {}: {error}",
                path.display()
            ))
        })?;
        if read == 0 {
            break;
        }

        // Leave an in-progress append for the next synchronization pass.
        if !bytes.ends_with(b"\n") {
            break;
        }
        processed_offset = processed_offset.saturating_add(read as i64);
        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        if bytes.is_empty() {
            continue;
        }

        let event = match serde_json::from_slice::<SidebarUsageEvent>(&bytes) {
            Ok(event) => event,
            Err(error) => {
                result.skipped = result.skipped.saturating_add(1);
                result.errors.push(format!(
                    "Codex sidebar usage JSON parse failed at byte {}: {error}",
                    processed_offset.saturating_sub(read as i64)
                ));
                continue;
            }
        };

        if let Err(error) = event.validate() {
            result.skipped = result.skipped.saturating_add(1);
            result.errors.push(format!(
                "Codex sidebar usage event {} skipped: {error}",
                event.event_id
            ));
            continue;
        }

        events.push(event);
    }

    // Match the v3.19.2 session importer: write one spool pass in a transaction
    // and advance its byte cursor in the same commit. A failed commit therefore
    // cannot leave the cursor ahead of the imported rows.
    let conn = lock_conn!(db.conn);
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| AppError::Database(format!("start Codex sidebar batch: {error}")))?;
    let mut pricing = HashMap::new();
    for event in &events {
        match insert_sidebar_usage_on_conn(&tx, event, &mut pricing) {
            Ok(true) => result.imported = result.imported.saturating_add(1),
            Ok(false) => result.skipped = result.skipped.saturating_add(1),
            Err(error) => {
                result.skipped = result.skipped.saturating_add(1);
                result
                    .errors
                    .push(format!("Codex sidebar event {}: {error}", event.event_id));
            }
        }
    }
    update_sync_state_on_conn(&tx, &path_key, modified, processed_offset)?;
    tx.commit()
        .map_err(|error| AppError::Database(format!("commit Codex sidebar batch: {error}")))?;

    if result.imported > 0 {
        log::info!(
            "[CODEX-SIDEBAR-SYNC] imported {}, skipped {}",
            result.imported,
            result.skipped
        );
    }
    Ok(result)
}

fn insert_sidebar_usage_on_conn(
    conn: &rusqlite::Connection,
    event: &SidebarUsageEvent,
    pricing_cache: &mut HashMap<String, Option<ModelPricing>>,
) -> Result<bool, AppError> {
    let usage = event
        .token_usage()
        .map_err(|error| AppError::Config(format!("invalid sidebar usage: {error}")))?;
    if !usage.has_billable_tokens() {
        return Ok(false);
    }

    let created_at = if event.completed_at_ms > 0 {
        event.completed_at_ms / 1000
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    };
    let model = if event.model.trim().is_empty() {
        "unknown"
    } else {
        event.model.trim()
    };
    let dedup_key = DedupKey {
        app_type: "codex",
        model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        cache_creation_tokens_known: true,
        created_at,
    };

    if merge_reasoning_into_matching_proxy_log(conn, &dedup_key)? {
        return Ok(false);
    }
    if merge_reasoning_into_matching_codex_session(conn, event, &usage, model, created_at)? {
        return Ok(false);
    }
    if should_skip_session_insert(conn, &event.event_id, &dedup_key)? {
        return Ok(false);
    }

    let pricing = pricing_cache
        .entry(model.to_string())
        .or_insert_with(|| find_model_pricing(conn, model));
    let costs =
        CostCalculator::try_calculate_for_app("codex", &usage, pricing.as_ref(), Decimal::ONE);
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) = costs
        .map(|cost| {
            (
                cost.input_cost.to_string(),
                cost.output_cost.to_string(),
                cost.cache_read_cost.to_string(),
                cost.cache_creation_cost.to_string(),
                cost.total_cost.to_string(),
            )
        })
        .unwrap_or_else(|| {
            (
                "0".to_string(),
                "0".to_string(),
                "0".to_string(),
                "0".to_string(),
                "0".to_string(),
            )
        });

    let inserted = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO proxy_request_logs (
                request_id, provider_id, app_type, model, request_model, pricing_model,
                input_tokens, output_tokens, reasoning_output_tokens,
                cache_read_tokens, cache_creation_tokens, input_token_semantics,
                input_cost_usd, output_cost_usd, cache_read_cost_usd,
                cache_creation_cost_usd, total_cost_usd,
                latency_ms, first_token_ms, status_code, error_message, session_id,
                provider_type, is_streaming, cost_multiplier, created_at, data_source
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
             )",
        )
        .and_then(|mut statement| {
            statement.execute(rusqlite::params![
                event.event_id,
                "_codex_sidebar",
                "codex",
                model,
                model,
                model,
                usage.input_tokens,
                usage.output_tokens,
                usage.reasoning_output_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                INPUT_TOKEN_SEMANTICS_TOTAL,
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                0i64,
                Option::<i64>::None,
                200i64,
                Option::<String>::None,
                event.thread_id,
                Some("codex_sidebar"),
                1i64,
                "1.0",
                created_at,
                "codex_sidebar",
            ])
        })
        .map_err(|error| {
            AppError::Database(format!("insert Codex sidebar usage failed: {error}"))
        })?;
    Ok(inserted > 0)
}

/// Prefer the rollout-derived row when a nominally ephemeral thread is also
/// persisted by Codex. The thread id plus the exact token fingerprint prevents
/// an unrelated request in the same time window from suppressing the sidebar
/// event. A missing reasoning count on the older row is completed in place.
fn merge_reasoning_into_matching_codex_session(
    conn: &rusqlite::Connection,
    event: &SidebarUsageEvent,
    usage: &TokenUsage,
    model: &str,
    created_at: i64,
) -> Result<bool, AppError> {
    let matching = conn
        .prepare_cached(
            "SELECT request_id, reasoning_output_tokens
               FROM proxy_request_logs
              WHERE app_type = 'codex'
                AND data_source = 'codex_session'
                AND session_id = ?1
                AND input_tokens = ?3
                AND output_tokens = ?4
                AND (reasoning_output_tokens = ?5 OR reasoning_output_tokens = 0 OR ?5 = 0)
                AND cache_read_tokens = ?6
                AND (cache_creation_tokens = ?7 OR cache_creation_tokens = 0)
                AND created_at BETWEEN ?8 - ?9 AND ?8 + ?9
                AND (
                    LOWER(model) = LOWER(?2)
                    OR LOWER(model) = 'unknown'
                    OR LOWER(?2) = 'unknown'
                )
              ORDER BY ABS(created_at - ?8), created_at
              LIMIT 1",
        )
        .and_then(|mut statement| {
            statement
                .query_row(
                    rusqlite::params![
                        event.thread_id,
                        model,
                        usage.input_tokens as i64,
                        usage.output_tokens as i64,
                        usage.reasoning_output_tokens as i64,
                        usage.cache_read_tokens as i64,
                        usage.cache_creation_tokens as i64,
                        created_at,
                        SESSION_PROXY_DEDUP_WINDOW_SECONDS,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?.max(0) as u32,
                        ))
                    },
                )
                .optional()
        })
        .map_err(|error| {
            AppError::Database(format!(
                "query matching Codex session for sidebar usage failed: {error}"
            ))
        })?;

    let Some((request_id, existing_reasoning)) = matching else {
        return Ok(false);
    };
    if usage.reasoning_output_tokens > existing_reasoning {
        conn.execute(
            "UPDATE proxy_request_logs
                SET reasoning_output_tokens = ?2
              WHERE request_id = ?1 AND reasoning_output_tokens < ?2",
            rusqlite::params![request_id, usage.reasoning_output_tokens as i64],
        )
        .map_err(|error| {
            AppError::Database(format!(
                "complete matching Codex session reasoning tokens failed: {error}"
            ))
        })?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn event(event_id: &str, input: u64, output: u64, reasoning: u64) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "eventId": event_id,
            "threadId": "thread-side",
            "turnId": "turn-side",
            "model": "gpt-5.6-sol",
            "completedAtMs": 1_800_000_000_000i64,
            "usage": {
                "inputTokens": input,
                "cachedInputTokens": 100,
                "cacheWriteInputTokens": 5,
                "outputTokens": output,
                "reasoningOutputTokens": reasoning,
                "totalTokens": input + output
            }
        })
    }

    fn append_value(path: &Path, value: &serde_json::Value) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{value}").unwrap();
    }

    #[test]
    fn imports_each_sampling_event_once() -> Result<(), AppError> {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sidebar.jsonl");
        append_value(
            &path,
            &event(
                "codex_sidebar:usage-v1:thread-side:turn-side:1",
                1000,
                80,
                40,
            ),
        );
        append_value(
            &path,
            &event(
                "codex_sidebar:usage-v1:thread-side:turn-side:2",
                1200,
                90,
                50,
            ),
        );
        let db = Database::memory()?;

        let first = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(first.imported, 2);
        assert_eq!(
            get_sync_state(&db, &path.to_string_lossy())?.1,
            fs::metadata(&path).unwrap().len() as i64
        );
        let second = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(second.imported, 0);

        append_value(
            &path,
            &event(
                "codex_sidebar:usage-v1:thread-side:turn-side:2",
                1200,
                90,
                50,
            ),
        );
        let replay = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(replay.imported, 0);

        let conn = lock_conn!(db.conn);
        let row: (i64, i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*), SUM(output_tokens), SUM(reasoning_output_tokens),
                    MIN(input_token_semantics)
               FROM proxy_request_logs
              WHERE data_source = 'codex_sidebar'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(row, (2, 170, 90, INPUT_TOKEN_SEMANTICS_TOTAL));
        Ok(())
    }

    #[test]
    fn skips_zero_and_incomplete_events_without_estimation() -> Result<(), AppError> {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sidebar.jsonl");
        let mut zero = event("codex_sidebar:usage-v1:thread-side:turn-side:1", 0, 0, 0);
        zero["usage"]["cachedInputTokens"] = serde_json::json!(0);
        zero["usage"]["cacheWriteInputTokens"] = serde_json::json!(0);
        append_value(&path, &zero);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"schemaVersion\":1").unwrap();

        let db = Database::memory()?;
        let first = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(first.imported, 0);
        assert_eq!(first.skipped, 1);

        writeln!(file, ",\"eventId\":\"broken\"}}").unwrap();
        drop(file);
        let second = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped, 1);
        Ok(())
    }

    #[test]
    fn matching_proxy_wins_and_receives_reasoning_count() -> Result<(), AppError> {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sidebar.jsonl");
        append_value(
            &path,
            &event(
                "codex_sidebar:usage-v1:thread-side:turn-side:1",
                1000,
                80,
                516,
            ),
        );
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO proxy_request_logs (
                    request_id, provider_id, app_type, model,
                    input_tokens, output_tokens, reasoning_output_tokens,
                    cache_read_tokens, cache_creation_tokens,
                    latency_ms, status_code, created_at, data_source
                 ) VALUES (?1, ?2, 'codex', 'gpt-5.6-sol', 1000, 80, 0, 100, 5,
                           0, 200, 1800000000, 'proxy')",
                rusqlite::params!["proxy-response", "provider"],
            )?;
        }

        let result = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(result.imported, 0);
        let conn = lock_conn!(db.conn);
        let row: (i64, i64) = conn.query_row(
            "SELECT COUNT(*), MAX(reasoning_output_tokens) FROM proxy_request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(row, (1, 516));
        Ok(())
    }

    #[test]
    fn matching_codex_session_wins_and_receives_reasoning_count() -> Result<(), AppError> {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sidebar.jsonl");
        append_value(
            &path,
            &event(
                "codex_sidebar:usage-v1:thread-side:turn-side:1",
                1000,
                80,
                516,
            ),
        );
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO proxy_request_logs (
                    request_id, provider_id, app_type, model,
                    input_tokens, output_tokens, reasoning_output_tokens,
                    cache_read_tokens, cache_creation_tokens,
                    latency_ms, status_code, session_id, created_at, data_source
                 ) VALUES (?1, '_codex_session', 'codex', 'gpt-5.6-sol',
                           1000, 80, 0, 100, 0, 0, 200, 'thread-side',
                           1800000000, 'codex_session')",
                rusqlite::params!["codex_session:thread-v1:thread-side:1"],
            )?;
        }

        let result = sync_codex_sidebar_usage_file(&db, &path)?;
        assert_eq!(result.imported, 0);
        let conn = lock_conn!(db.conn);
        let row: (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN data_source = 'codex_sidebar' THEN 1 ELSE 0 END),
                    MAX(reasoning_output_tokens)
               FROM proxy_request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(row, (1, 0, 516));
        Ok(())
    }
}
