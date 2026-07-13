use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult};
use hmac::{Hmac, Mac};
use integration::signing::{BybitCredentials, BybitSigner};
use serde_json::Value;
use sha2::Sha256;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type HmacSha256 = Hmac<Sha256>;

struct Command {
    id: String,
    payload: Value,
    deadline: Instant,
    reply: oneshot::Sender<HftResult<Value>>,
}

#[derive(Clone)]
pub struct BybitWsOrderClient {
    tx: mpsc::Sender<Command>,
    timeout: Duration,
}

impl BybitWsOrderClient {
    pub async fn connect(
        url: String,
        credentials: BybitCredentials,
        timeout: Duration,
    ) -> HftResult<Self> {
        let socket = connect_socket(&url, &credentials).await?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(run(url, credentials, socket, rx));
        Ok(Self { tx, timeout })
    }

    pub async fn submit(&self, id: String, payload: Value) -> HftResult<Value> {
        let (reply, response) = oneshot::channel();
        let deadline = Instant::now() + self.timeout;
        tokio::time::timeout_at(
            deadline,
            self.tx.send(Command {
                id,
                payload,
                deadline,
                reply,
            }),
        )
        .await
        .map_err(|_| HftError::Timeout("Bybit WS order expired before queueing".to_string()))?
        .map_err(|_| HftError::Network("Bybit WS order channel stopped".to_string()))?;
        tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
            .await
            .map_err(|_| HftError::Timeout("Bybit WS order response timed out".to_string()))?
            .map_err(|_| HftError::Network("Bybit WS order response dropped".to_string()))?
    }
}

fn auth_payload(credentials: &BybitCredentials) -> Value {
    let expires = BybitSigner::current_timestamp().saturating_add(1_000);
    let mut mac = HmacSha256::new_from_slice(credentials.secret_key.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(format!("GET/realtime{expires}").as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    serde_json::json!({
        "op": "auth",
        "args": [credentials.api_key, expires, signature],
    })
}

async fn connect_socket(url: &str, credentials: &BybitCredentials) -> HftResult<Socket> {
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| HftError::Network(format!("Bybit WS order connect: {error}")))?;
    integration::ws::set_ws_tcp_nodelay(socket.get_ref(), true)
        .map_err(|error| HftError::Network(format!("Bybit WS order TCP_NODELAY: {error}")))?;
    socket
        .send(Message::Text(auth_payload(credentials).to_string().into()))
        .await
        .map_err(|error| HftError::Network(format!("Bybit WS order auth send: {error}")))?;
    let value = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|error| HftError::Serialization(error.to_string()))?;
                    if value.get("op").and_then(Value::as_str) == Some("auth") {
                        return Ok(value);
                    }
                }
                Some(Ok(Message::Ping(payload))) => socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| HftError::Network(error.to_string()))?,
                Some(Ok(Message::Close(_))) | None => {
                    return Err(HftError::Network(
                        "Bybit WS order closed during auth".to_string(),
                    ));
                }
                Some(Err(error)) => return Err(HftError::Network(error.to_string())),
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| HftError::Timeout("Bybit WS order auth timed out".to_string()))??;
    if value.get("retCode").and_then(Value::as_i64) != Some(0) {
        return Err(HftError::Authentication(format!(
            "Bybit WS order auth rejected: {value}"
        )));
    }
    Ok(socket)
}

async fn run(
    url: String,
    credentials: BybitCredentials,
    initial: Socket,
    mut rx: mpsc::Receiver<Command>,
) {
    let mut socket = Some(initial);
    let mut backoff = Duration::from_millis(100);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    heartbeat.tick().await;
    loop {
        if socket.is_none() {
            match connect_socket(&url, &credentials).await {
                Ok(connected) => {
                    socket = Some(connected);
                    backoff = Duration::from_millis(100);
                }
                Err(error) => {
                    tracing::warn!(%error, "Bybit WS order reconnect failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }

        let ws = socket.as_mut().expect("socket checked above");
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else { return; };
                let remaining = command.deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let _ = command.reply.send(Err(HftError::Timeout(
                        "Bybit WS order expired before send".to_string(),
                    )));
                    continue;
                }
                let result = send_request(ws, &command.id, command.payload, remaining).await;
                if result.is_err() {
                    socket = None;
                }
                let _ = command.reply.send(result);
            }
            _ = heartbeat.tick() => {
                if ws.send(Message::Text(serde_json::json!({"op": "ping"}).to_string().into())).await.is_err() {
                    socket = None;
                }
            }
            message = ws.next() => {
                match message {
                    Some(Ok(Message::Ping(payload))) => {
                        if ws.send(Message::Pong(payload)).await.is_err() {
                            socket = None;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => socket = None,
                    _ => {}
                }
            }
        }
    }
}

async fn send_request(
    socket: &mut Socket,
    id: &str,
    payload: Value,
    timeout: Duration,
) -> HftResult<Value> {
    socket
        .send(Message::Text(payload.to_string().into()))
        .await
        .map_err(|error| {
            HftError::Network(format!("Bybit WS order send outcome unknown: {error}"))
        })?;
    tokio::time::timeout(timeout, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|error| HftError::Serialization(error.to_string()))?;
                    if value.get("reqId").and_then(Value::as_str) == Some(id) {
                        return Ok(value);
                    }
                }
                Some(Ok(Message::Ping(payload))) => socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| HftError::Network(error.to_string()))?,
                Some(Ok(Message::Close(_))) | None => {
                    return Err(HftError::Network(
                        "Bybit WS order closed with outcome unknown".to_string(),
                    ));
                }
                Some(Err(error)) => {
                    return Err(HftError::Network(format!(
                        "Bybit WS order read outcome unknown: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| HftError::Timeout("Bybit WS order outcome unknown".to_string()))?
}
