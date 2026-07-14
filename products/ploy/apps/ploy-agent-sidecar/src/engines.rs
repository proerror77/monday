use crate::{AgentTaskCompletion, Result, SidecarError};
use ploy_operator_contracts::AgentToolCallRecord;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const MAX_CODEX_STREAM_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CODEX_FINAL_BYTES: u64 = 1024 * 1024;
const MAX_XAI_RESPONSE_BYTES: u64 = 1024 * 1024;
const CODEX_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "code_mode",
    "multi_agent",
    "plugins",
    "plugin_sharing",
    "tool_suggest",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "in_app_browser",
    "computer_use",
    "workspace_dependencies",
    "image_generation",
    "apps",
    "standalone_web_search",
    "hooks",
];
const CODEX_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "CODEX_HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "SystemRoot",
    "ComSpec",
    "PATHEXT",
];

#[derive(Debug)]
pub struct CodexResult {
    pub session_id: String,
    pub value: Value,
    pub tool_calls: Vec<AgentToolCallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokContext {
    pub provider: String,
    pub model: String,
    pub summary: String,
}

pub fn query_codex_strategy_completion(
    objective: &str,
    run_packet: &str,
    run_contract: &str,
    runtime_context: &Value,
    harness_context: &str,
    focused_subagents: &Value,
    grok_context: Option<&GrokContext>,
) -> Result<(AgentTaskCompletion, CodexResult)> {
    let prompt = [
        "You are the Codex CLI execution engine for the Ploy Strategy Builder sidecar.\nReturn one JSON object only. Do not submit orders, apply deployments, or modify files.",
        &format!("Objective:\n{objective}"),
        &format!("Run packet:\n{run_packet}"),
        &format!("Run contract:\n{run_contract}"),
        &format!(
            "Runtime context:\n{}",
            truncate(&runtime_context.to_string(), 6_000)
        ),
        &format!(
            "Focused subagent findings:\n{}",
            truncate(&focused_subagents.to_string(), 4_000)
        ),
        &format!(
            "Grok API context:\n{}",
            truncate(
                &serde_json::to_string(&grok_context).unwrap_or_else(|_| "null".to_string()),
                3_000,
            )
        ),
        &format!("Harness context:\n{}", truncate(harness_context, 4_000)),
        "Return JSON with keys: status (success|partial|blocked), summary, decision, grok_decision, evidence array, blockers array, next_action. Use grok_decision not_queried when no Grok decision was required.",
    ]
    .join("\n\n");
    let result = run_codex_exec(&prompt, completion_schema())?;
    let completion = parse_completion(&result.value)?;
    Ok((completion, result))
}

pub fn query_codex_focused_subagent(
    profile: &str,
    prompt: &str,
    runtime_context: &Value,
    harness_context: &str,
) -> Result<(AgentTaskCompletion, CodexResult)> {
    let prompt = [
        &format!("Focused subagent profile: {profile}"),
        "Return one JSON object only. Do not mutate deployments or files.",
        prompt,
        &format!(
            "Runtime context:\n{}",
            truncate(&runtime_context.to_string(), 6_000)
        ),
        &format!("Harness context:\n{}", truncate(harness_context, 4_000)),
        "Return JSON with keys: status (success|partial|blocked), summary, decision, grok_decision, evidence array, blockers array, next_action.",
    ]
    .join("\n\n");
    let result = run_codex_exec(&prompt, completion_schema())?;
    let completion = parse_completion(&result.value)?;
    Ok((completion, result))
}

pub fn query_codex_scan(
    timestamp: &str,
    runtime_context: &Value,
    harness_context: &str,
) -> Result<CodexResult> {
    let prompt = [
        &format!("Current time: {timestamp}"),
        "Run a dry, operator-facing NBA comeback scan from the available runtime context. Return structured JSON only. Do not submit orders, apply deployments, or modify files.",
        &format!(
            "Runtime context:\n{}",
            truncate(&runtime_context.to_string(), 8_000)
        ),
        &format!("Harness context:\n{}", truncate(harness_context, 4_000)),
    ]
    .join("\n\n");
    run_codex_exec(&prompt, scan_schema())
}

pub fn query_grok_builder_context(
    objective: &str,
    run_packet: &str,
    run_contract: &str,
) -> Result<Option<GrokContext>> {
    if xai_api_key().is_none() {
        return Ok(None);
    }
    let text = query_xai_text(
        "You are Grok Builder evidence synthesis for a trading harness. Return compact evidence only. Do not recommend live orders.",
        &format!(
            "Objective:\n{objective}\n\nRun packet:\n{run_packet}\n\nRun contract:\n{run_contract}\n\nReturn: grok_decision candidate (trade/pass/not_queried), evidence gaps, and confidence."
        ),
    )?;
    Ok(Some(GrokContext {
        provider: "xai".to_string(),
        model: text.0,
        summary: text.1,
    }))
}

pub fn query_grok_strategy_completion(
    objective: &str,
    run_packet: &str,
    run_contract: &str,
    runtime_context: &Value,
    harness_context: &str,
) -> Result<(AgentTaskCompletion, GrokContext)> {
    let (model, text) = query_xai_text(
        "You are the xAI/Grok execution engine for a diagnostic trading harness. Return one JSON object only. Do not submit orders or request deployments.",
        &format!(
            "Objective:\n{objective}\n\nRun packet:\n{run_packet}\n\nRun contract:\n{run_contract}\n\nRuntime context:\n{}\n\nHarness context:\n{}\n\nReturn JSON with keys status, summary, decision, grok_decision, evidence, blockers, next_action.",
            truncate(&runtime_context.to_string(), 6_000),
            truncate(harness_context, 4_000),
        ),
    )?;
    let value = parse_json_object(&text)?;
    let completion = parse_completion(&value)?;
    Ok((
        completion.clone(),
        GrokContext {
            provider: "xai".to_string(),
            model,
            summary: completion.summary,
        },
    ))
}

fn run_codex_exec(prompt: &str, schema: Value) -> Result<CodexResult> {
    let temporary = tempfile::Builder::new()
        .prefix("ploy-codex-cli-")
        .tempdir()?;
    let result = run_codex_exec_inner(prompt, schema, temporary.path());
    match (result, temporary.close()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(cleanup)) => Err(SidecarError::Message(format!(
            "remove private codex working directory: {cleanup}"
        ))),
        (Err(primary), Err(cleanup)) => Err(SidecarError::Message(format!(
            "{primary}; remove private codex working directory: {cleanup}"
        ))),
    }
}

fn run_codex_exec_inner(prompt: &str, schema: Value, temporary: &Path) -> Result<CodexResult> {
    let schema_path = temporary.join("schema.json");
    let output_path = temporary.join("last-message.json");
    fs::write(&schema_path, serde_json::to_vec(&schema)?)?;
    let model = nonempty_env("CODEX_CLI_MODEL");
    let args = codex_args(temporary, &schema_path, &output_path, model.as_deref());

    let binary = nonempty_env("CODEX_CLI_BIN").unwrap_or_else(|| "codex".to_string());
    let timeout = Duration::from_secs(env_u64("CODEX_CLI_TIMEOUT_SECS", 600));
    let started = Instant::now();
    let mut command = Command::new(&binary);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for key in CODEX_CHILD_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| SidecarError::Message(format!("spawn {binary}: {err}")))?;
    let child_group_id = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = spawn_stream_reader(stdout);
    let stderr_reader = spawn_stream_reader(stderr);
    let stdin_writer = child
        .stdin
        .take()
        .map(|stdin| spawn_stdin_writer(stdin, prompt.as_bytes().to_vec()));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => {
                terminate_child(&mut child);
                return Err(err.into());
            }
        }
        if started.elapsed() >= timeout {
            terminate_child(&mut child);
            return Err(SidecarError::Message("codex exec timed out".to_string()));
        }
        thread::sleep(Duration::from_millis(50));
    };
    kill_process_group(child_group_id);
    if let Some(stdin_writer) = stdin_writer {
        receive_stdin(stdin_writer, started, timeout)?;
    }
    let stdout = receive_stream(stdout_reader, "stdout", started, timeout)?;
    let stderr = receive_stream(stderr_reader, "stderr", started, timeout)?;
    if !status.success() {
        let detail = if stderr.trim().is_empty() {
            &stdout
        } else {
            &stderr
        };
        return Err(SidecarError::Message(format!(
            "codex exec exited {:?}: {}",
            status.code(),
            truncate(detail.trim(), 4_000)
        )));
    }
    if let Some(forbidden) = forbidden_codex_event(&stdout) {
        return Err(SidecarError::Message(format!(
            "codex event stream rejected: {forbidden}"
        )));
    }
    let value = parse_json_object(&read_limited_file(&output_path, MAX_CODEX_FINAL_BYTES)?)?;
    Ok(CodexResult {
        session_id: "codex-cli".to_string(),
        value,
        tool_calls: parse_codex_tool_calls(&stdout),
    })
}

fn codex_args(
    workdir: &Path,
    schema_path: &Path,
    output_path: &Path,
    model: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["--ask-for-approval".to_string(), "never".to_string()];
    for feature in CODEX_DISABLED_FEATURES {
        args.extend(["--disable".to_string(), (*feature).to_string()]);
    }
    args.extend([
        "-c".to_string(),
        "mcp_servers={}".to_string(),
        "-c".to_string(),
        "shell_environment_policy.inherit=\"none\"".to_string(),
        "--strict-config".to_string(),
        "exec".to_string(),
        "--ignore-user-config".to_string(),
        "--ignore-rules".to_string(),
        "--skip-git-repo-check".to_string(),
        "--json".to_string(),
        "--ephemeral".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--sandbox".to_string(),
        "read-only".to_string(),
        "-C".to_string(),
        workdir.to_string_lossy().into_owned(),
        "--output-schema".to_string(),
        schema_path.to_string_lossy().into_owned(),
        "--output-last-message".to_string(),
        output_path.to_string_lossy().into_owned(),
    ]);
    if let Some(model) = model {
        args.extend(["-m".to_string(), model.to_string()]);
    }
    args.push("-".to_string());
    args
}

fn read_stream(mut stream: impl Read) -> std::io::Result<String> {
    let mut output = Vec::new();
    stream
        .by_ref()
        .take(MAX_CODEX_STREAM_BYTES + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > MAX_CODEX_STREAM_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "codex output exceeded 8 MiB",
        ));
    }
    String::from_utf8(output)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

fn spawn_stream_reader(stream: impl Read + Send + 'static) -> Receiver<std::io::Result<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_stream(stream));
    });
    receiver
}

fn spawn_stdin_writer(
    mut stdin: impl Write + Send + 'static,
    prompt: Vec<u8>,
) -> Receiver<std::io::Result<()>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(stdin.write_all(&prompt));
    });
    receiver
}

fn receive_stream(
    receiver: Receiver<std::io::Result<String>>,
    name: &str,
    started: Instant,
    timeout: Duration,
) -> Result<String> {
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or_else(|| SidecarError::Message(format!("codex {name} output pipe timed out")))?;
    match receiver.recv_timeout(remaining) {
        Ok(output) => Ok(output?),
        Err(RecvTimeoutError::Timeout) => Err(SidecarError::Message(format!(
            "codex {name} output pipe timed out"
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(SidecarError::Message(format!(
            "codex {name} output reader stopped"
        ))),
    }
}

fn receive_stdin(
    receiver: Receiver<std::io::Result<()>>,
    started: Instant,
    timeout: Duration,
) -> Result<()> {
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or_else(|| SidecarError::Message("codex stdin pipe timed out".to_string()))?;
    match receiver.recv_timeout(remaining) {
        Ok(output) => Ok(output?),
        Err(RecvTimeoutError::Timeout) => Err(SidecarError::Message(
            "codex stdin pipe timed out".to_string(),
        )),
        Err(RecvTimeoutError::Disconnected) => Err(SidecarError::Message(
            "codex stdin writer stopped".to_string(),
        )),
    }
}

fn read_limited_file(path: &Path, max_bytes: u64) -> Result<String> {
    let file = fs::File::open(path)?;
    read_limited_utf8(file, max_bytes, "codex final output")
}

fn read_limited_utf8(reader: impl Read, max_bytes: u64, label: &str) -> Result<String> {
    let mut bytes = Vec::new();
    reader.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(SidecarError::Message(format!(
            "{label} exceeded {max_bytes} bytes"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|err| SidecarError::Message(format!("{label} was not UTF-8: {err}")))
}

fn terminate_child(child: &mut Child) {
    kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn kill_process_group(group_id: u32) {
    let Ok(group_id) = i32::try_from(group_id) else {
        return;
    };
    // SAFETY: the child was placed in a new process group whose id is its pid.
    unsafe {
        libc::kill(-group_id, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_group_id: u32) {}

fn query_xai_text(system: &str, user: &str) -> Result<(String, String)> {
    let api_key = xai_api_key()
        .ok_or_else(|| SidecarError::Message("xAI/Grok API key is not configured".to_string()))?;
    let endpoint = nonempty_env("XAI_CHAT_COMPLETIONS_URL")
        .unwrap_or_else(|| "https://api.x.ai/v1/chat/completions".to_string());
    validate_xai_endpoint(&endpoint)?;
    let model = nonempty_env("XAI_MODEL").unwrap_or_else(|| "grok-4.5".to_string());
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(env_u64("XAI_TIMEOUT_SECS", 90)))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "temperature": 0.2,
            "max_tokens": 4096,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ]
        }))
        .send()?;
    let status = response.status();
    let body = read_limited_utf8(response, MAX_XAI_RESPONSE_BYTES, "xAI response")?;
    if !status.is_success() {
        return Err(SidecarError::Message(format!(
            "xAI Grok API failed: {status} {}",
            truncate(&body, 2_000)
        )));
    }
    let value: Value = serde_json::from_str(&body)?;
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Grok returned no text")
        .to_string();
    Ok((model, text))
}

fn validate_xai_endpoint(endpoint: &str) -> Result<()> {
    if endpoint.starts_with("https://") {
        Ok(())
    } else {
        Err(SidecarError::Message(
            "XAI_CHAT_COMPLETIONS_URL must use HTTPS".to_string(),
        ))
    }
}

pub fn parse_codex_tool_calls(stdout: &str) -> Vec<AgentToolCallRecord> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| {
            let item = event
                .get("item")
                .or_else(|| event.pointer("/data/item"))
                .unwrap_or(&event);
            let name = if item.get("type").and_then(Value::as_str) == Some("mcp_tool_call") {
                format!(
                    "mcp__{}__{}",
                    item.get("server")?.as_str()?,
                    item.get("tool")?.as_str()?
                )
            } else {
                item.get("tool_name")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("tool"))?
                    .as_str()?
                    .to_string()
            };
            let event_type = event
                .get("type")
                .or_else(|| item.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !event_type.contains("completed") && !event_type.contains("tool") {
                return None;
            }
            let failed = event_type.contains("failed")
                || item.get("status").and_then(Value::as_str) == Some("failed")
                || nonempty_error(item.get("error"));
            Some(AgentToolCallRecord {
                name,
                status: if failed { "failed" } else { "completed" }.to_string(),
            })
        })
        .collect()
}

fn forbidden_codex_event(stdout: &str) -> Option<String> {
    stdout.lines().enumerate().find_map(|(index, line)| {
        if line.trim().is_empty() {
            return None;
        }
        let event = match serde_json::from_str::<Value>(line) {
            Ok(event) => event,
            Err(err) => {
                return Some(format!("malformed JSON event on line {}: {err}", index + 1));
            }
        };
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Some(format!("event on line {} has no type", index + 1));
        };
        let item = event.get("item").or_else(|| event.pointer("/data/item"));
        let item_type = item
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str);
        match event_type {
            "thread.started" | "turn.started" | "turn.completed" => None,
            "item.started" | "item.updated" | "item.completed"
                if matches!(item_type, Some("reasoning" | "agent_message")) =>
            {
                None
            }
            "item.started" | "item.updated" | "item.completed" => Some(format!(
                "forbidden item type {}",
                item_type.unwrap_or("unknown_item")
            )),
            "error" | "turn.failed" => Some(format!("failure event `{event_type}`")),
            _ => Some(format!("unknown event type `{event_type}`")),
        }
    })
}

fn nonempty_error(error: Option<&Value>) -> bool {
    match error {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

fn parse_completion(value: &Value) -> Result<AgentTaskCompletion> {
    let status = required_enum(value, "status", &["success", "partial", "blocked"])?;
    let summary = required_string(value, "summary")?;
    let decision = required_enum(
        value,
        "decision",
        &["continue", "pass", "trade", "monitor", "blocked"],
    )?;
    let grok_decision = required_enum(value, "grok_decision", &["trade", "pass", "not_queried"])?;
    Ok(AgentTaskCompletion {
        status,
        summary,
        decision: Some(decision),
        grok_decision: Some(grok_decision),
        evidence: required_string_array(value, "evidence")?,
        blockers: required_string_array(value, "blockers")?,
        next_action: Some(required_string(value, "next_action")?),
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| SidecarError::Message(format!("model response missing {field}")))
}

fn required_enum(value: &Value, field: &str, allowed: &[&str]) -> Result<String> {
    let candidate = required_string(value, field)?;
    if allowed.contains(&candidate.as_str()) {
        Ok(candidate)
    } else {
        Err(SidecarError::Message(format!(
            "model response has invalid {field}"
        )))
    }
}

fn required_string_array(value: &Value, field: &str) -> Result<Vec<String>> {
    let items = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| SidecarError::Message(format!("model response missing {field}")))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| SidecarError::Message(format!("model response has invalid {field}")))
        })
        .collect()
}

fn parse_json_object(text: &str) -> Result<Value> {
    if let Ok(value @ Value::Object(_)) = serde_json::from_str(text) {
        return Ok(value);
    }
    if let (Some(first), Some(last)) = (text.find('{'), text.rfind('}')) {
        if first < last {
            let value: Value = serde_json::from_str(&text[first..=last])?;
            if value.is_object() {
                return Ok(value);
            }
        }
    }
    Err(SidecarError::Message(
        "model did not return a JSON object".to_string(),
    ))
}

fn completion_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "status": { "type": "string", "enum": ["success", "partial", "blocked"] },
            "summary": { "type": "string" },
            "decision": { "type": "string", "enum": ["continue", "pass", "trade", "monitor", "blocked"] },
            "grok_decision": { "type": "string", "enum": ["trade", "pass", "not_queried"] },
            "evidence": { "type": "array", "items": { "type": "string" } },
            "blockers": { "type": "array", "items": { "type": "string" } },
            "next_action": { "type": "string" }
        },
        "required": ["status", "summary", "decision", "grok_decision", "evidence", "blockers", "next_action"]
    })
}

fn scan_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scan_summary": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "games_scanned": { "type": "number" },
                    "in_progress_games": { "type": "number" },
                    "comeback_candidates": { "type": "number" },
                    "markets_checked": { "type": "number" },
                    "timestamp": { "type": "string" }
                },
                "required": ["games_scanned", "in_progress_games", "comeback_candidates", "markets_checked", "timestamp"]
            },
            "opportunities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "game_id": { "type": "string" },
                        "game_name": { "type": "string" },
                        "trailing_team": { "type": "string" },
                        "trailing_abbrev": { "type": "string" },
                        "deficit": { "type": "number" },
                        "quarter": { "type": "number" },
                        "clock": { "type": "string" },
                        "market_slug": { "type": "string" },
                        "market_price": { "type": "number" },
                        "reward_risk_ratio": { "type": "number" },
                        "estimated_win_prob": { "type": "number" },
                        "expected_value": { "type": "number" },
                        "kelly_fraction": { "type": "number" },
                        "action": { "type": "string", "enum": ["TRADE", "PASS", "MONITOR"] },
                        "grok_decision": { "type": "string", "enum": ["trade", "pass", "not_queried"] },
                        "confidence": { "type": "string", "enum": ["low", "medium", "high"] },
                        "reasoning": { "type": "string" },
                        "risk_factors": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": [
                        "game_id", "game_name", "trailing_team", "trailing_abbrev", "deficit",
                        "quarter", "clock", "market_slug", "market_price", "reward_risk_ratio",
                        "estimated_win_prob", "expected_value", "kelly_fraction", "action",
                        "grok_decision", "confidence", "reasoning", "risk_factors"
                    ]
                }
            },
            "operator_actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "kind": { "type": "string" },
                        "target": { "type": "string" },
                        "status": { "type": "string" },
                        "details": { "type": "string" }
                    },
                    "required": ["kind", "target", "status", "details"]
                }
            }
        },
        "required": ["scan_summary", "opportunities", "operator_actions"]
    })
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn xai_api_key() -> Option<String> {
    nonempty_env("XAI_API_KEY").or_else(|| nonempty_env("GROK_API_KEY"))
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_u64(key: &str, default: u64) -> u64 {
    nonempty_env(key)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_receipts_preserve_mcp_success_and_failure() {
        let calls = parse_codex_tool_calls(
            r#"{"type":"item.completed","item":{"type":"mcp_tool_call","server":"research","tool":"run_backtest","error":null}}
{"type":"item.completed","item":{"type":"mcp_tool_call","server":"research","tool":"compare_configs","error":{"message":"denied"}}}"#,
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "mcp__research__run_backtest");
        assert_eq!(calls[0].status, "completed");
        assert_eq!(calls[1].status, "failed");
    }

    #[test]
    fn codex_invocation_is_prompt_only_and_exec_flags_follow_subcommand() {
        let args = codex_args(
            Path::new("/tmp/evidence"),
            Path::new("/tmp/evidence/schema.json"),
            Path::new("/tmp/evidence/output.json"),
            None,
        );
        let exec = args.iter().position(|arg| arg == "exec").expect("exec");
        let ignore = args
            .iter()
            .position(|arg| arg == "--ignore-user-config")
            .expect("ignore config");
        assert!(ignore > exec);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--disable", "shell_tool"]));
        assert!(args.iter().any(|arg| arg == "mcp_servers={}"));
        assert!(!args.iter().any(|arg| arg == "--search"));
    }

    #[test]
    fn codex_event_parser_rejects_every_tool_surface() {
        assert_eq!(
            forbidden_codex_event(
                r#"{"type":"item.completed","item":{"type":"command_execution","command":"env"}}"#
            )
            .as_deref(),
            Some("forbidden item type command_execution")
        );
        assert_eq!(
            forbidden_codex_event(
                r#"{"type":"item.completed","item":{"type":"file_change","path":"secret"}}"#
            )
            .as_deref(),
            Some("forbidden item type file_change")
        );
        assert!(forbidden_codex_event(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#
        )
        .is_none());
        assert!(forbidden_codex_event("not-json")
            .as_deref()
            .is_some_and(|reason| reason.contains("malformed JSON")));
        assert_eq!(
            forbidden_codex_event(r#"{"type":"future.lifecycle"}"#).as_deref(),
            Some("unknown event type `future.lifecycle`")
        );
    }

    #[test]
    fn completion_parser_rejects_incomplete_or_invalid_output() {
        assert!(parse_completion(&json!({
            "status": "unknown",
            "summary": "done",
            "decision": "live_trade",
            "evidence": ["receipt"]
        }))
        .is_err());
        assert!(parse_json_object("plain-text model response").is_err());
    }

    #[test]
    fn completion_parser_accepts_the_strict_contract() {
        let completion = parse_completion(&json!({
            "status": "blocked",
            "summary": "missing evidence",
            "decision": "blocked",
            "grok_decision": "not_queried",
            "evidence": [],
            "blockers": ["no quote"],
            "next_action": "collect quote"
        }))
        .expect("strict completion");
        assert_eq!(completion.status, "blocked");
        assert_eq!(completion.decision.as_deref(), Some("blocked"));
        assert_eq!(completion.blockers, vec!["no quote"]);
    }

    #[test]
    fn output_reader_obeys_the_child_deadline() {
        let (_sender, receiver) = mpsc::channel();
        let error = receive_stream(receiver, "stdout", Instant::now(), Duration::from_millis(1))
            .expect_err("open pipe must time out");
        assert!(error.to_string().contains("output pipe timed out"));
    }

    #[test]
    fn grok_endpoint_is_https_only() {
        assert!(validate_xai_endpoint("https://api.x.ai/v1/chat/completions").is_ok());
        assert!(validate_xai_endpoint("http://api.x.ai/v1/chat/completions").is_err());
    }

    #[test]
    fn grok_response_reader_is_bounded() {
        let oversized = vec![b'x'; MAX_XAI_RESPONSE_BYTES as usize + 1];
        assert!(read_limited_utf8(
            std::io::Cursor::new(oversized),
            MAX_XAI_RESPONSE_BYTES,
            "xAI response"
        )
        .is_err());
    }
}
