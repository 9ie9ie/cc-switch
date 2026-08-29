//! Transparent Codex CLI launcher that records request-level token usage
//! for active ephemeral app-server threads. It does not proxy OpenAI traffic or
//! persist prompts, responses, credentials, or tool payloads.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const REAL_CLI_PATH_ENV: &str = "CC_SWITCH_CODEX_REAL_CLI_PATH";
const USAGE_PATH_ENV: &str = "CC_SWITCH_CODEX_SIDEBAR_USAGE_PATH";
const USAGE_FILE: &str = "codex-sidebar-usage.jsonl";
const EVENT_ID_PREFIX: &str = "codex_sidebar:usage-v1:";
const TIMING_EVENT_ID_PREFIX: &str = "codex_sidebar:timing-v1:";

fn main() {
    match run() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("cc-switch Codex bridge failed: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> io::Result<ExitStatus> {
    let args: Vec<String> = env::args().skip(1).collect();
    let real_cli = resolve_real_cli()?;
    if !args.iter().any(|arg| arg == "app-server") {
        return Command::new(real_cli).args(args).status();
    }

    run_app_server(real_cli, &args)
}

fn run_app_server(real_cli: PathBuf, args: &[String]) -> io::Result<ExitStatus> {
    let mut child = Command::new(real_cli)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("real Codex stdin unavailable"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("real Codex stdout unavailable"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("real Codex stderr unavailable"))?;
    let state = Arc::new(Mutex::new(BridgeState::default()));

    let input_state = Arc::clone(&state);
    thread::spawn(move || {
        let _ = forward_client_input(io::stdin(), child_stdin, input_state);
    });
    thread::spawn(move || {
        let mut stderr = io::stderr().lock();
        let mut reader = BufReader::new(child_stderr);
        let _ = io::copy(&mut reader, &mut stderr);
    });

    forward_server_output(child_stdout, io::stdout(), state)?;
    wait_for_child(&mut child)
}

fn wait_for_child(child: &mut Child) -> io::Result<ExitStatus> {
    child.wait()
}

fn forward_client_input<R: Read, W: Write>(
    input: R,
    mut child_stdin: W,
    state: Arc<Mutex<BridgeState>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(input);
    loop {
        let mut bytes = Vec::new();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(trim_line_ending(&bytes)) {
            lock_state(&state).observe_client_message(&value);
        }
        child_stdin.write_all(&bytes)?;
        child_stdin.flush()?;
    }
    Ok(())
}

fn forward_server_output<R: Read, W: Write>(
    output: R,
    stdout: W,
    state: Arc<Mutex<BridgeState>>,
) -> io::Result<()> {
    let mut reader = BufReader::new(output);
    let mut stdout = io::BufWriter::new(stdout);
    loop {
        let mut bytes = Vec::new();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }

        // Forward first so usage persistence never delays the desktop UI.
        stdout.write_all(&bytes)?;
        stdout.flush()?;

        let record = serde_json::from_slice::<Value>(trim_line_ending(&bytes))
            .ok()
            .and_then(|value| lock_state(&state).observe_server_message(&value));
        if let Some(record) = record {
            if let Err(error) = append_record(&record) {
                eprintln!("cc-switch Codex bridge could not persist telemetry: {error}");
            }
        }
    }
    Ok(())
}

fn trim_line_ending(bytes: &[u8]) -> &[u8] {
    let without_lf = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

fn lock_state(state: &Arc<Mutex<BridgeState>>) -> MutexGuard<'_, BridgeState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Default)]
struct BridgeState {
    pending: HashMap<String, PendingRequest>,
    threads: HashMap<String, ThreadInfo>,
    turns: HashMap<String, TurnInfo>,
}

#[derive(Debug)]
enum PendingRequest {
    Thread {
        ephemeral_hint: bool,
        model_hint: Option<String>,
    },
    Turn {
        thread_id: String,
        model_override: Option<String>,
        started_at: Instant,
    },
}

#[derive(Debug, Clone)]
struct ThreadInfo {
    ephemeral: bool,
    model: String,
}

#[derive(Debug, Clone)]
struct TurnInfo {
    thread_id: String,
    model: String,
    sequence: u64,
    last_total: Option<UsageSignature>,
    started_at: Instant,
    first_token_ms: Option<u64>,
    timing_emitted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageSignature {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UsageBreakdown {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl UsageBreakdown {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            input_tokens: nonnegative_u64(value.get("inputTokens")?)?,
            cached_input_tokens: nonnegative_u64(value.get("cachedInputTokens")?)?,
            cache_write_input_tokens: value
                .get("cacheWriteInputTokens")
                .and_then(nonnegative_u64)
                .unwrap_or(0),
            output_tokens: nonnegative_u64(value.get("outputTokens")?)?,
            reasoning_output_tokens: nonnegative_u64(value.get("reasoningOutputTokens")?)?,
            total_tokens: nonnegative_u64(value.get("totalTokens")?)?,
        })
    }

    fn signature(self) -> UsageSignature {
        UsageSignature {
            input_tokens: self.input_tokens,
            cached_input_tokens: self.cached_input_tokens,
            cache_write_input_tokens: self.cache_write_input_tokens,
            output_tokens: self.output_tokens,
            reasoning_output_tokens: self.reasoning_output_tokens,
            total_tokens: self.total_tokens,
        }
    }

    fn has_billable_tokens(self) -> bool {
        self.input_tokens > 0
            || self.cached_input_tokens > 0
            || self.cache_write_input_tokens > 0
            || self.output_tokens > 0
            || self.reasoning_output_tokens > 0
    }
}

fn nonnegative_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_i64()
            .filter(|value| *value >= 0)
            .map(|value| value as u64)
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UsageRecord {
    schema_version: u32,
    event_id: String,
    thread_id: String,
    turn_id: String,
    model: String,
    completed_at_ms: i64,
    usage: UsageBreakdown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TimingRecord {
    schema_version: u32,
    event_id: String,
    target_event_id: String,
    thread_id: String,
    turn_id: String,
    completed_at_ms: i64,
    duration_ms: u64,
    first_token_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
enum BridgeRecord {
    Usage(UsageRecord),
    Timing(TimingRecord),
}

impl BridgeState {
    fn observe_client_message(&mut self, value: &Value) {
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return;
        };
        let Some(id) = value.get("id").and_then(request_key) else {
            return;
        };

        match method {
            "thread/start" | "thread/fork" => {
                let params = value.get("params").unwrap_or(&Value::Null);
                let ephemeral_hint = params
                    .get("ephemeral")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let model_hint = params
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.pending.insert(
                    id,
                    PendingRequest::Thread {
                        ephemeral_hint,
                        model_hint,
                    },
                );
            }
            "turn/start" => {
                let Some(params) = value.get("params") else {
                    return;
                };
                let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
                    return;
                };
                let model_override = params
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.pending.insert(
                    id,
                    PendingRequest::Turn {
                        thread_id: thread_id.to_owned(),
                        model_override,
                        started_at: Instant::now(),
                    },
                );
            }
            _ => {}
        }
    }

    fn observe_server_message(&mut self, value: &Value) -> Option<BridgeRecord> {
        if let Some(id) = value.get("id").and_then(request_key) {
            self.observe_response(&id, value);
            return None;
        }

        let method = value.get("method").and_then(Value::as_str)?;
        match method {
            "thread/started" => self.observe_thread_started(value),
            "thread/settings/updated" => self.observe_thread_settings(value),
            "thread/closed" | "thread/deleted" => self.observe_thread_closed(value),
            "turn/started" => self.observe_turn_started(value),
            "item/agentMessage/delta" => self.observe_agent_message_delta(value),
            "turn/completed" => return self.capture_timing(value).map(BridgeRecord::Timing),
            "model/rerouted" => self.observe_model_rerouted(value),
            "thread/tokenUsage/updated" => {
                return self.capture_usage(value).map(BridgeRecord::Usage)
            }
            _ => {}
        }
        None
    }

    fn observe_response(&mut self, id: &str, value: &Value) {
        let Some(pending) = self.pending.remove(id) else {
            return;
        };
        let Some(result) = value.get("result") else {
            return;
        };

        match pending {
            PendingRequest::Thread {
                ephemeral_hint,
                model_hint,
            } => {
                let Some(thread_id) = result.pointer("/thread/id").and_then(Value::as_str) else {
                    return;
                };
                let ephemeral = result
                    .pointer("/thread/ephemeral")
                    .and_then(Value::as_bool)
                    .unwrap_or(ephemeral_hint);
                let model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(model_hint)
                    .unwrap_or_else(|| "unknown".to_string());
                self.threads
                    .insert(thread_id.to_owned(), ThreadInfo { ephemeral, model });
            }
            PendingRequest::Turn {
                thread_id,
                model_override,
                started_at,
            } => {
                let Some(turn_id) = result.pointer("/turn/id").and_then(Value::as_str) else {
                    return;
                };
                let model = model_override
                    .or_else(|| {
                        self.threads
                            .get(&thread_id)
                            .map(|thread| thread.model.clone())
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                self.turns
                    .entry(turn_id.to_owned())
                    .and_modify(|turn| {
                        turn.thread_id = thread_id.clone();
                        turn.model = model.clone();
                    })
                    .or_insert(TurnInfo {
                        thread_id,
                        model,
                        sequence: 0,
                        last_total: None,
                        started_at,
                        first_token_ms: None,
                        timing_emitted: false,
                    });
            }
        }
    }

    fn observe_thread_started(&mut self, value: &Value) {
        let Some(thread_id) = value.pointer("/params/thread/id").and_then(Value::as_str) else {
            return;
        };
        let ephemeral = value
            .pointer("/params/thread/ephemeral")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.threads
            .entry(thread_id.to_owned())
            .and_modify(|thread| thread.ephemeral = ephemeral)
            .or_insert_with(|| ThreadInfo {
                ephemeral,
                model: "unknown".to_string(),
            });
    }

    fn observe_thread_settings(&mut self, value: &Value) {
        let Some(thread_id) = value.pointer("/params/threadId").and_then(Value::as_str) else {
            return;
        };
        let Some(model) = value
            .pointer("/params/threadSettings/model")
            .and_then(Value::as_str)
        else {
            return;
        };
        if let Some(thread) = self.threads.get_mut(thread_id) {
            thread.model = model.to_owned();
        }
    }

    fn observe_thread_closed(&mut self, value: &Value) {
        let Some(thread_id) = value.pointer("/params/threadId").and_then(Value::as_str) else {
            return;
        };
        self.threads.remove(thread_id);
        self.turns.retain(|_, turn| turn.thread_id != thread_id);
    }

    fn observe_turn_started(&mut self, value: &Value) {
        let Some(thread_id) = value.pointer("/params/threadId").and_then(Value::as_str) else {
            return;
        };
        let Some(turn_id) = value.pointer("/params/turn/id").and_then(Value::as_str) else {
            return;
        };
        let model = self
            .threads
            .get(thread_id)
            .map(|thread| thread.model.clone())
            .unwrap_or_else(|| "unknown".to_string());
        self.turns
            .entry(turn_id.to_owned())
            .and_modify(|turn| {
                if turn.sequence == 0 && turn.first_token_ms.is_none() {
                    turn.started_at = Instant::now();
                }
                turn.thread_id = thread_id.to_owned();
                turn.model = model.clone();
            })
            .or_insert(TurnInfo {
                thread_id: thread_id.to_owned(),
                model,
                sequence: 0,
                last_total: None,
                started_at: Instant::now(),
                first_token_ms: None,
                timing_emitted: false,
            });
    }

    fn observe_agent_message_delta(&mut self, value: &Value) {
        let Some(turn_id) = value.pointer("/params/turnId").and_then(Value::as_str) else {
            return;
        };
        let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) else {
            return;
        };
        if delta.is_empty() {
            return;
        }
        if let Some(turn) = self.turns.get_mut(turn_id) {
            if turn.first_token_ms.is_none() {
                turn.first_token_ms =
                    Some(turn.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64);
            }
        }
    }

    fn capture_timing(&mut self, value: &Value) -> Option<TimingRecord> {
        let thread_id = value.pointer("/params/threadId").and_then(Value::as_str)?;
        let turn_id = value.pointer("/params/turn/id").and_then(Value::as_str)?;
        if !self
            .threads
            .get(thread_id)
            .is_some_and(|thread| thread.ephemeral)
        {
            return None;
        }
        let turn = self.turns.get_mut(turn_id)?;
        if turn.thread_id != thread_id || turn.sequence == 0 || turn.timing_emitted {
            return None;
        }
        let duration_ms = value
            .pointer("/params/turn/durationMs")
            .and_then(nonnegative_u64)
            .unwrap_or_else(|| turn.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64);
        let completed_at_ms = value
            .pointer("/params/turn/completedAt")
            .and_then(nonnegative_u64)
            .and_then(|value| value.checked_mul(1000))
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or_else(now_millis);
        turn.timing_emitted = true;
        Some(TimingRecord {
            schema_version: 1,
            event_id: format!("{TIMING_EVENT_ID_PREFIX}{thread_id}:{turn_id}"),
            target_event_id: format!("{EVENT_ID_PREFIX}{thread_id}:{turn_id}:{}", turn.sequence),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            completed_at_ms,
            duration_ms,
            first_token_ms: turn.first_token_ms,
        })
    }

    fn observe_model_rerouted(&mut self, value: &Value) {
        let Some(turn_id) = value.pointer("/params/turnId").and_then(Value::as_str) else {
            return;
        };
        let Some(model) = value.pointer("/params/toModel").and_then(Value::as_str) else {
            return;
        };
        if let Some(turn) = self.turns.get_mut(turn_id) {
            turn.model = model.to_owned();
        }
    }

    fn capture_usage(&mut self, value: &Value) -> Option<UsageRecord> {
        let thread_id = value.pointer("/params/threadId").and_then(Value::as_str)?;
        let turn_id = value.pointer("/params/turnId").and_then(Value::as_str)?;
        if !self
            .threads
            .get(thread_id)
            .is_some_and(|thread| thread.ephemeral)
        {
            return None;
        }

        let total = UsageBreakdown::from_value(value.pointer("/params/tokenUsage/total")?)?;
        let last = UsageBreakdown::from_value(value.pointer("/params/tokenUsage/last")?)?;
        let turn = self.turns.get_mut(turn_id)?;
        if turn.thread_id != thread_id {
            return None;
        }

        let signature = total.signature();
        if turn.last_total == Some(signature) {
            return None;
        }
        turn.last_total = Some(signature);
        if !last.has_billable_tokens() {
            return None;
        }

        turn.sequence = turn.sequence.saturating_add(1);
        let completed_at_ms = value
            .get("emittedAtMs")
            .and_then(nonnegative_u64)
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or_else(now_millis);
        Some(UsageRecord {
            schema_version: 1,
            event_id: format!("{EVENT_ID_PREFIX}{thread_id}:{turn_id}:{}", turn.sequence),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            model: turn.model.clone(),
            completed_at_ms,
            usage: last,
        })
    }
}

fn request_key(value: &Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn append_record(record: &BridgeRecord) -> io::Result<()> {
    let Some(path) = usage_path() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "USERPROFILE/HOME is unavailable and no usage path override was set",
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
    bytes.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&bytes)
}

fn usage_path() -> Option<PathBuf> {
    env::var_os(USAGE_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .map(PathBuf::from)
                .map(|home| home.join(".cc-switch").join(USAGE_FILE))
        })
}

fn resolve_real_cli() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os(REAL_CLI_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        return validate_real_cli(path);
    }

    #[cfg(windows)]
    if let Some(parent) = parent_process_executable() {
        if let Some(app_dir) = parent.parent() {
            let candidate = app_dir.join("resources").join("codex.exe");
            if candidate.is_file() {
                return validate_real_cli(candidate);
            }
        }
    }

    #[cfg(windows)]
    if let Some(candidate) = npm_codex_candidate() {
        if candidate.is_file() {
            return validate_real_cli(candidate);
        }
    }

    #[cfg(windows)]
    if let Ok(output) = Command::new("where.exe").arg("codex.exe").output() {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let candidate = PathBuf::from(line.trim());
                if candidate.is_file() && !is_current_executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        return Ok(PathBuf::from("codex"));
    }

    #[cfg(windows)]
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("real Codex CLI not found; set {REAL_CLI_PATH_ENV}"),
    ))
}

#[cfg(windows)]
fn npm_codex_candidate() -> Option<PathBuf> {
    let app_data = env::var_os("APPDATA").map(PathBuf::from)?;
    let package = if cfg!(target_arch = "aarch64") {
        "codex-win32-arm64"
    } else {
        "codex-win32-x64"
    };
    let target = if cfg!(target_arch = "aarch64") {
        "aarch64-pc-windows-msvc"
    } else {
        "x86_64-pc-windows-msvc"
    };
    Some(
        app_data
            .join("npm")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join(package)
            .join("vendor")
            .join(target)
            .join("bin")
            .join("codex.exe"),
    )
}

fn validate_real_cli(path: PathBuf) -> io::Result<PathBuf> {
    if is_current_executable(&path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REAL_CLI_PATH_ENV} points back to the bridge"),
        ));
    }
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("real Codex CLI does not exist: {}", path.display()),
        ));
    }
    Ok(path)
}

fn is_current_executable(path: &Path) -> bool {
    let Ok(current) = env::current_exe() else {
        return false;
    };
    match (current.canonicalize(), path.canonicalize()) {
        (Ok(current), Ok(candidate)) => current == candidate,
        _ => current == path,
    }
}

#[cfg(windows)]
fn parent_process_executable() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        let current_pid = GetCurrentProcessId();
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut parent_pid = 0;
        let mut has_entry = Process32FirstW(snapshot, &mut entry) != 0;
        while has_entry {
            if entry.th32ProcessID == current_pid {
                parent_pid = entry.th32ParentProcessID;
                break;
            }
            has_entry = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        if parent_pid == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent_pid);
        if process.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) != 0;
        CloseHandle(process);
        if !ok || size == 0 {
            return None;
        }
        buffer.truncate(size as usize);
        Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_usage(record: BridgeRecord) -> UsageRecord {
        match record {
            BridgeRecord::Usage(record) => record,
            BridgeRecord::Timing(_) => panic!("expected usage record"),
        }
    }

    fn expect_timing(record: BridgeRecord) -> TimingRecord {
        match record {
            BridgeRecord::Timing(record) => record,
            BridgeRecord::Usage(_) => panic!("expected timing record"),
        }
    }

    fn fork_side_thread(state: &mut BridgeState) {
        state.observe_client_message(&serde_json::json!({
            "method": "thread/fork",
            "id": 1,
            "params": {"threadId": "parent", "ephemeral": true, "model": "gpt-5.6-sol"}
        }));
        state.observe_server_message(&serde_json::json!({
            "id": 1,
            "result": {
                "thread": {"id": "side", "ephemeral": true},
                "model": "gpt-5.6-sol"
            }
        }));
    }

    fn start_turn(state: &mut BridgeState) {
        state.observe_client_message(&serde_json::json!({
            "method": "turn/start",
            "id": 2,
            "params": {"threadId": "side", "input": []}
        }));
        state.observe_server_message(&serde_json::json!({
            "id": 2,
            "result": {"turn": {"id": "turn-1"}}
        }));
    }

    fn usage(total_input: u64, last_output: u64, reasoning: u64) -> Value {
        serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "emittedAtMs": 1_800_000_000_000i64,
            "params": {
                "threadId": "side",
                "turnId": "turn-1",
                "tokenUsage": {
                    "total": {
                        "inputTokens": total_input,
                        "cachedInputTokens": 100,
                        "cacheWriteInputTokens": 5,
                        "outputTokens": 300,
                        "reasoningOutputTokens": 150,
                        "totalTokens": total_input + 300
                    },
                    "last": {
                        "inputTokens": 1000,
                        "cachedInputTokens": 100,
                        "cacheWriteInputTokens": 5,
                        "outputTokens": last_output,
                        "reasoningOutputTokens": reasoning,
                        "totalTokens": 1000 + last_output
                    },
                    "modelContextWindow": 120192
                }
            }
        })
    }

    #[test]
    fn ignores_inherited_usage_and_records_each_new_sampling_event() {
        let mut state = BridgeState::default();
        fork_side_thread(&mut state);

        // Fork replay references a turn that this bridge did not observe starting.
        let inherited = serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "side",
                "turnId": "parent-turn",
                "tokenUsage": {
                    "total": {"inputTokens": 5000, "cachedInputTokens": 4000,
                              "cacheWriteInputTokens": 0, "outputTokens": 500,
                              "reasoningOutputTokens": 200, "totalTokens": 5500},
                    "last": {"inputTokens": 5000, "cachedInputTokens": 4000,
                             "cacheWriteInputTokens": 0, "outputTokens": 500,
                             "reasoningOutputTokens": 200, "totalTokens": 5500}
                }
            }
        });
        assert!(state.observe_server_message(&inherited).is_none());

        start_turn(&mut state);
        let first = expect_usage(state.observe_server_message(&usage(6000, 80, 40)).unwrap());
        assert_eq!(first.event_id, "codex_sidebar:usage-v1:side:turn-1:1");
        assert_eq!(first.usage.reasoning_output_tokens, 40);

        assert!(state.observe_server_message(&usage(6000, 80, 40)).is_none());
        let second = expect_usage(state.observe_server_message(&usage(7000, 100, 60)).unwrap());
        assert_eq!(second.event_id, "codex_sidebar:usage-v1:side:turn-1:2");
        assert_eq!(second.usage.output_tokens, 100);
    }

    #[test]
    fn ignores_non_ephemeral_threads() {
        let mut state = BridgeState::default();
        state.observe_client_message(&serde_json::json!({
            "method": "thread/start", "id": "root", "params": {"ephemeral": false}
        }));
        state.observe_server_message(&serde_json::json!({
            "id": "root",
            "result": {"thread": {"id": "side", "ephemeral": false}, "model": "gpt-5.6-sol"}
        }));
        start_turn(&mut state);
        assert!(state.observe_server_message(&usage(6000, 80, 40)).is_none());
    }

    #[test]
    fn uses_rerouted_model_and_persists_no_message_text() {
        let mut state = BridgeState::default();
        fork_side_thread(&mut state);
        start_turn(&mut state);
        state.observe_server_message(&serde_json::json!({
            "method": "model/rerouted",
            "params": {"threadId": "side", "turnId": "turn-1",
                       "fromModel": "gpt-5.6-sol", "toModel": "gpt-5.6-terra"}
        }));
        let record = expect_usage(state.observe_server_message(&usage(6000, 80, 40)).unwrap());
        assert_eq!(record.model, "gpt-5.6-terra");
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("prompt"));
        assert!(!json.contains("response"));
        assert!(!json.contains("input\""));
    }

    #[test]
    fn records_turn_timing_for_the_final_usage_event_without_message_text() {
        let mut state = BridgeState::default();
        fork_side_thread(&mut state);
        start_turn(&mut state);
        let first = expect_usage(state.observe_server_message(&usage(6000, 80, 40)).unwrap());
        let second = expect_usage(state.observe_server_message(&usage(7000, 100, 60)).unwrap());
        assert_ne!(first.event_id, second.event_id);

        state.observe_server_message(&serde_json::json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "side",
                "turnId": "turn-1",
                "itemId": "message-1",
                "delta": "首字"
            }
        }));
        let timing = expect_timing(
            state
                .observe_server_message(&serde_json::json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "side",
                        "turn": {
                            "id": "turn-1",
                            "status": "completed",
                            "items": [],
                            "completedAt": 1_800_000_010i64,
                            "durationMs": 4_200i64
                        }
                    }
                }))
                .unwrap(),
        );
        assert_eq!(timing.target_event_id, second.event_id);
        assert_eq!(timing.duration_ms, 4_200);
        assert!(timing.first_token_ms.is_some());
        let json = serde_json::to_string(&BridgeRecord::Timing(timing)).unwrap();
        assert!(!json.contains("首字"));
        assert!(!json.contains("prompt"));
        assert!(!json.contains("response"));
        assert!(state
            .observe_server_message(&serde_json::json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "side",
                    "turn": {"id": "turn-1", "status": "completed", "items": [], "durationMs": 4_200}
                }
            }))
            .is_none());
    }
}
