use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
pub struct BinanceWsOrderAttempt {
    pub outcome: HftResult<Value>,
    pub write_started_mono_us: Option<u64>,
    pub write_returned_mono_us: Option<u64>,
    pub decoded_response_mono_us: Option<u64>,
}

struct Command {
    id: String,
    payload: Value,
    deadline: Instant,
    reply: oneshot::Sender<HftResult<BinanceWsOrderAttempt>>,
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

    pub async fn submit(&self, id: String, payload: Value) -> HftResult<BinanceWsOrderAttempt> {
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
                if result.outcome.is_err() {
                    socket = None;
                }
                let _ = command.reply.send(Ok(result));
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
) -> BinanceWsOrderAttempt {
    let message = Message::Text(payload.to_string().into());
    let write_started_mono_us = hft_core::monotonic_micros();
    if let Err(error) = socket.send(message).await {
        return BinanceWsOrderAttempt {
            outcome: Err(HftError::Network(format!(
                "Binance WS order send outcome unknown: {error}"
            ))),
            write_started_mono_us: Some(write_started_mono_us),
            write_returned_mono_us: None,
            decoded_response_mono_us: None,
        };
    }
    let write_returned_mono_us = hft_core::monotonic_micros();

    let response = tokio::time::timeout(timeout, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    let value: Value = match serde_json::from_str(&text) {
                        Ok(value) => value,
                        Err(error) => {
                            return (Err(HftError::Serialization(error.to_string())), None)
                        }
                    };
                    if value.get("id").and_then(Value::as_str) == Some(id) {
                        let response_received_mono_us = hft_core::monotonic_micros();
                        return (Ok(value), Some(response_received_mono_us));
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(error) = socket.send(Message::Pong(payload)).await {
                        return (Err(HftError::Network(error.to_string())), None);
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    return (
                        Err(HftError::Network(
                            "Binance WS order closed with outcome unknown".to_string(),
                        )),
                        None,
                    );
                }
                Some(Err(error)) => {
                    return (
                        Err(HftError::Network(format!(
                            "Binance WS order read outcome unknown: {error}"
                        ))),
                        None,
                    );
                }
                _ => {}
            }
        }
    })
    .await;
    let (outcome, decoded_response_mono_us) = response.unwrap_or_else(|_| {
        (
            Err(HftError::Timeout(
                "Binance WS order outcome unknown".to_string(),
            )),
            None,
        )
    });
    BinanceWsOrderAttempt {
        outcome,
        write_started_mono_us: Some(write_started_mono_us),
        write_returned_mono_us: Some(write_returned_mono_us),
        decoded_response_mono_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn delayed_response_is_separate_from_userspace_write_return() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let request = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = request else {
                panic!("expected text request");
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            socket
                .send(Message::Text(
                    serde_json::json!({"id": request["id"], "status": 200})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });

        let client =
            BinanceWsOrderClient::connect(format!("ws://{address}"), Duration::from_millis(200))
                .await
                .unwrap();
        let receipt = client
            .submit(
                "client-42".to_string(),
                serde_json::json!({"id": "client-42"}),
            )
            .await
            .unwrap();

        assert!(receipt.outcome.is_ok());
        let write_started = receipt.write_started_mono_us.unwrap();
        let write_returned = receipt.write_returned_mono_us.unwrap();
        let response_received = receipt.decoded_response_mono_us.unwrap();
        assert!(write_started <= write_returned);
        assert!(write_returned < response_received);
        assert!(
            response_received - write_returned >= 10_000,
            "delayed venue response must not be folded into userspace write"
        );
    }

    #[tokio::test]
    async fn timeout_after_write_preserves_userspace_boundaries() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _request = socket.next().await.unwrap().unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let client =
            BinanceWsOrderClient::connect(format!("ws://{address}"), Duration::from_millis(20))
                .await
                .unwrap();
        let attempt = client
            .submit(
                "client-timeout".to_string(),
                serde_json::json!({"id": "client-timeout"}),
            )
            .await
            .unwrap();

        assert!(matches!(attempt.outcome, Err(HftError::Timeout(_))));
        assert!(attempt.write_started_mono_us.is_some());
        assert!(attempt.write_returned_mono_us.is_some());
        assert!(attempt.decoded_response_mono_us.is_none());
    }

    #[tokio::test]
    async fn malformed_response_is_not_a_validated_response_boundary() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _request = socket.next().await.unwrap().unwrap();
            socket.send(Message::Text("not-json".into())).await.unwrap();
        });

        let client =
            BinanceWsOrderClient::connect(format!("ws://{address}"), Duration::from_millis(200))
                .await
                .unwrap();
        let attempt = client
            .submit(
                "client-malformed".to_string(),
                serde_json::json!({"id": "client-malformed"}),
            )
            .await
            .unwrap();

        assert!(matches!(attempt.outcome, Err(HftError::Serialization(_))));
        assert!(attempt.write_started_mono_us.is_some());
        assert!(attempt.write_returned_mono_us.is_some());
        assert!(attempt.decoded_response_mono_us.is_none());
    }
}
