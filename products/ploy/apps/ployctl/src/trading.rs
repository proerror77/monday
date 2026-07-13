use crate::client::ControlPlaneClient;
use ploy_operator_contracts::OrderReplaceRequest;

pub fn render_trading_state(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .trading_state()?
        .into_iter()
        .map(|state| {
            format!(
                "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} reserved_exposure={} total_exposure={} net_pnl={}",
                state.deployment_id,
                state.runtime_mode,
                state.intents.len(),
                state.orders.len(),
                state.fills.len(),
                state.positions.len(),
                state.risk.active_orders,
                state.risk.gross_exposure,
                state.risk.reserved_order_exposure,
                state.risk.total_gross_exposure,
                state.pnl.net_pnl,
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_one_trading_state(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
    client.inspect_trading_state(deployment_id).map(|state| {
        format!(
            "{} runtime={} intents={} orders={} fills={} positions={} active_orders={} gross_exposure={} reserved_exposure={} total_exposure={} net_pnl={}",
            state.deployment_id,
            state.runtime_mode,
            state.intents.len(),
            state.orders.len(),
            state.fills.len(),
            state.positions.len(),
            state.risk.active_orders,
            state.risk.gross_exposure,
            state.risk.reserved_order_exposure,
            state.risk.total_gross_exposure,
            state.pnl.net_pnl,
        )
    })
}

pub fn cancel_order(
    client: &ControlPlaneClient,
    deployment_id: &str,
    order_id: &str,
) -> Result<String, String> {
    client
        .cancel_order(deployment_id, order_id)
        .map(|response| {
            format!(
                "{} order={} state={} filled_qty={} venue_order_id={}",
                response.deployment_id,
                response.order_id,
                response.state,
                response.filled_qty,
                response.venue_order_id.unwrap_or_else(|| "-".to_string()),
            )
        })
}

pub fn replace_order(
    client: &ControlPlaneClient,
    deployment_id: &str,
    order_id: &str,
    quantity: rust_decimal::Decimal,
    limit_price: Option<rust_decimal::Decimal>,
) -> Result<String, String> {
    client
        .replace_order(
            deployment_id,
            order_id,
            &OrderReplaceRequest {
                quantity,
                limit_price,
            },
        )
        .map(|response| {
            format!(
                "{} order={} state={} revision={} filled_qty={} requested_qty={} limit_price={} venue_order_id={} history={}",
                response.deployment_id,
                response.order_id,
                response.state,
                response.revision,
                response.filled_qty,
                response.requested_qty,
                response
                    .limit_price
                    .map(|price| price.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                response.venue_order_id.unwrap_or_else(|| "-".to_string()),
                if response.venue_order_history.is_empty() {
                    "-".to_string()
                } else {
                    response.venue_order_history.join(",")
                },
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{cancel_order, render_one_trading_state, render_trading_state, replace_order};
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::{OrderControlResponse, TradingStateSnapshot};
    use rust_decimal::Decimal;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-trading-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_trading_state() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                ..TradingStateSnapshot::default()
            }])
            .expect("trading json"),
        )
        .expect("write trading state");

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        let output = render_trading_state(&client).expect("trading state");
        assert!(output.contains("example.paper"));
        assert!(output.contains("net_pnl=0"));
        assert!(render_one_trading_state(&client, "example.paper").is_ok());
    }

    #[test]
    fn renders_order_cancel_response() {
        let runtime_root = temp_dir("cancel");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "canceled".to_string(),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                requested_qty: Decimal::ZERO,
                limit_price: None,
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(runtime_root);
        client.control_plane_addr = addr.to_string();
        client.admin_token = None;
        client.operator_token = None;
        client.sidecar_token = None;
        let output = cancel_order(&client, "example.live", "order-1").expect("cancel output");
        assert!(output.contains("state=canceled"));
        assert!(output.contains("venue_order_id=venue-1"));
    }

    #[test]
    fn renders_order_replace_response() {
        let runtime_root = temp_dir("replace");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "acknowledged".to_string(),
                venue_order_id: Some("venue-2".to_string()),
                venue_order_history: vec!["venue-1".to_string()],
                revision: 1,
                requested_qty: Decimal::new(250, 2),
                limit_price: Some(Decimal::new(57, 2)),
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(runtime_root);
        client.control_plane_addr = addr.to_string();
        client.admin_token = None;
        client.operator_token = None;
        client.sidecar_token = None;
        let output = replace_order(
            &client,
            "example.live",
            "order-1",
            Decimal::new(250, 2),
            Some(Decimal::new(57, 2)),
        )
        .expect("replace output");
        assert!(output.contains("revision=1"));
        assert!(output.contains("requested_qty=2.50"));
        assert!(output.contains("limit_price=0.57"));
        assert!(output.contains("history=venue-1"));
    }
}
