use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Command {
    id: String,
    payload: Value,
    deadline: Instant,
    reply: oneshot::Sender<HftResult<Value>>,
}

#[derive(Clone)]
pub struct BinanceWsOrderClient {
    tx: mpsc::Sender<Command>,
    timeout: Duration,
}

impl BinanceWsOrderClient {
    pub async fn connect(url: String, timeout: Duration) -> HftResult<Self> {
        let socket = connect_socket(&url).await?;
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(run(url, socket, rx));
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
        .map_err(|_| HftError::Timeout("Binance WS order expired before queueing".to_string()))?
        .map_err(|_| HftError::Network("Binance WS order channel stopped".to_string()))?;
        tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
            .await
            .map_err(|_| HftError::Timeout("Binance WS order response timed out".to_string()))?
            .map_err(|_| HftError::Network("Binance WS order response dropped".to_string()))?
    }
}

async fn connect_socket(url: &str) -> HftResult<Socket> {
    let (socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| HftError::Network(format!("Binance WS order connect: {error}")))?;
    integration::ws::set_ws_tcp_nodelay(socket.get_ref(), true)
        .map_err(|error| HftError::Network(format!("Binance WS order TCP_NODELAY: {error}")))?;
    Ok(socket)
}

async fn run(url: String, initial: Socket, mut rx: mpsc::Receiver<Command>) {
    let mut socket = Some(initial);
    let mut backoff = Duration::from_millis(100);
    loop {
        if socket.is_none() {
            match connect_socket(&url).await {
                Ok(connected) => {
                    socket = Some(connected);
                    backoff = Duration::from_millis(100);
                }
                Err(error) => {
                    tracing::warn!(%error, "Binance WS order reconnect failed");
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
                        "Binance WS order expired before send".to_string(),
                    )));
                    continue;
                }
                let result = send_request(ws, &command.id, command.payload, remaining).await;
                if result.is_err() {
                    socket = None;
                }
                let _ = command.reply.send(result);
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
            HftError::Network(format!("Binance WS order send outcome unknown: {error}"))
        })?;

    tokio::time::timeout(timeout, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|error| HftError::Serialization(error.to_string()))?;
                    if value.get("id").and_then(Value::as_str) == Some(id) {
                        return Ok(value);
                    }
                }
                Some(Ok(Message::Ping(payload))) => socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| HftError::Network(error.to_string()))?,
                Some(Ok(Message::Close(_))) | None => {
                    return Err(HftError::Network(
                        "Binance WS order closed with outcome unknown".to_string(),
                    ));
                }
                Some(Err(error)) => {
                    return Err(HftError::Network(format!(
                        "Binance WS order read outcome unknown: {error}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| HftError::Timeout("Binance WS order outcome unknown".to_string()))?
}
