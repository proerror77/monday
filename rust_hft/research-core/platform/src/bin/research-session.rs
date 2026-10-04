//! Trusted Session host. Operator commands are typed; no generic RPC passthrough.
use anyhow::{bail, ensure, Context, Result};
use hft_research_platform::{
    coding_agent::{ApprovalKind, RpcId},
    postgres::{completion_message, Ledger},
    research::{CodingAgent, Session},
    session::{AppServer, NativeState, ResearchClient, SessionConfig},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::Read, path::Path};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    session: SessionConfig,
    tenant: String,
    experiment_sha256: String,
    capability_policy_receipt_sha256: String,
    research_endpoint: String,
    research_token_file: String,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Message {
        intent_sha256: String,
        text: String,
    },
    Reconcile {
        intent_sha256: String,
    },
    Interrupt {
        turn_id: String,
    },
    Approve {
        generation: String,
        id: RpcId,
        kind: ApprovalKind,
        response: Value,
    },
    NextEvent,
    Wake,
    Checkpoint,
    Close,
}

fn read<T: serde::de::DeserializeOwned>(path: &str) -> Result<T> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "configuration must be a regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds bound");
    Ok(serde_json::from_slice(&bytes)?)
}

fn print(value: &Value) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (config_path, native_path) = match args.as_slice() {
        [operation, config] if operation == "start" => (config.as_str(), None),
        [operation, config, native] if operation == "resume" => (config.as_str(), Some(native.as_str())),
        _ => bail!("usage: research-session start CONFIG | resume CONFIG NATIVE_STATE; typed operator commands on stdin"),
    };
    let config: Config = read(config_path)?;
    ensure!(
        !config.tenant.is_empty()
            && config.tenant.len() <= 128
            && Path::new(&config.research_token_file).is_absolute(),
        "invalid host principal or capability path"
    );
    let client = ResearchClient::new(
        &config.research_endpoint,
        hft_research_platform::service::read_secret(&config.research_token_file)?,
    )?;
    let ledger = Ledger::connect(
        &std::env::var("MONDAY_RESEARCH_DATABASE_URL").context("host PG identity required")?,
    )
    .await?;
    let executable_sha256 = config.session.executable_sha256.clone();
    let mut server = if let Some(path) = native_path {
        AppServer::resume(config.session, &read::<NativeState>(path)?).await?
    } else {
        let mut server = AppServer::start(config.session).await?;
        server.open_thread(None).await?;
        server
    };
    let session = Session {
        schema: 1,
        experiment_sha256: config.experiment_sha256,
        provider: CodingAgent::CodexAppServer,
        provider_version: hft_research_platform::coding_agent::SCHEMA_VERSION.into(),
        provider_thread_id: server.thread_id().context("native thread missing")?.into(),
        provider_binary_sha256: executable_sha256,
        capability_policy_receipt_sha256: config.capability_policy_receipt_sha256,
    };
    let session_id = ledger.register_session(&config.tenant, &session).await?;
    print(
        &json!({"session_sha256":session_id,"thread_id":server.thread_id(),"generation":server.generation()}),
    )?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut input_frame = Vec::new();
    let mut wake_timer = tokio::time::interval(std::time::Duration::from_secs(15));
    wake_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // Both readers retain partial frames when a timer or another input wins.
        let command = tokio::select! {
            frame = operator_frame(&mut input, &mut input_frame) => {
                let Some(frame) = frame? else {
                    print(&serde_json::to_value(server.checkpoint().await?)?)?;
                    return Ok(());
                };
                serde_json::from_slice::<Command>(&frame)?
            }
            event = server.next_event() => {
                let event = event?;
                if event.get("method").and_then(Value::as_str) == Some("item/tool/call") { server.answer_tool(&event, &client).await?; }
                print(&event)?;
                continue;
            }
            _ = wake_timer.tick() => {
                let result = wake(&ledger, &config.tenant, &session_id, &mut server).await;
                match result {
                    Ok(delivered) if !delivered.is_empty() => print(&json!({"native_delivery_read_back":delivered}))?,
                    Ok(_) => {},
                    Err(_) => print(&json!({"completion_delivery_pending":true}))?,
                }
                continue;
            }
        };
        let result = match command {
            Command::Message {
                intent_sha256,
                text,
            } => serde_json::to_value(server.send_message(&intent_sha256, &text).await?)?,
            Command::Reconcile { intent_sha256 } => {
                serde_json::to_value(server.reconcile_message(&intent_sha256).await?)?
            }
            Command::Interrupt { turn_id } => {
                server.interrupt(&turn_id).await?;
                json!({"session_interrupt_requested":true})
            }
            Command::Approve {
                generation,
                id,
                kind,
                response,
            } => {
                server.approve(&generation, &id, kind, response).await?;
                json!({"approval_replied":true})
            }
            Command::NextEvent => {
                let event = server.next_event().await?;
                if event.get("method").and_then(Value::as_str) == Some("item/tool/call") {
                    server.answer_tool(&event, &client).await?;
                }
                event
            }
            Command::Wake => {
                json!({"native_delivery_read_back":wake(&ledger, &config.tenant, &session_id, &mut server).await?})
            }
            Command::Checkpoint => {
                print(&serde_json::to_value(server.checkpoint().await?)?)?;
                return Ok(());
            }
            Command::Close => {
                server.close().await?;
                print(&json!({"child_stopped":true}))?;
                return Ok(());
            }
        };
        print(&result)?;
    }
}

async fn operator_frame(
    input: &mut BufReader<tokio::io::Stdin>,
    frame: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>> {
    loop {
        let bytes = input.fill_buf().await?;
        if bytes.is_empty() {
            ensure!(frame.is_empty(), "incomplete operator frame");
            return Ok(None);
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let n = end.map_or(bytes.len(), |i| i + 1);
        ensure!(
            frame.len() + n <= 64 * 1024,
            "operator command exceeds bound"
        );
        frame.extend_from_slice(&bytes[..n]);
        input.consume(n);
        if end.is_some() {
            return Ok(Some(std::mem::take(frame)));
        }
    }
}
async fn wake(
    ledger: &Ledger,
    tenant: &str,
    session: &str,
    server: &mut AppServer,
) -> Result<Vec<String>> {
    let mut delivered = Vec::new();
    for (id, intent) in ledger.pending_completions(tenant, session).await? {
        let message = completion_message(&id, &intent)?;
        // A prior Unknown record is reconciled below, never resent.
        let _ = server.send_message(&id, &message).await;
        let delivery = server.verified_delivery(&id).await?;
        ledger
            .record_completion_delivery(tenant, session, &delivery)
            .await?;
        delivered.push(id);
    }
    Ok(delivered)
}
