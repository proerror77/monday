//! Example IPC control client
//!
//! Demonstrates how to send commands to the HFT control plane

use hft_ipc::{IPCClient, Command, Response, DEFAULT_SOCKET_PATH};
use std::env;
use tracing::{info, error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::init();

    let socket_path = env::var("HFT_CONTROL_SOCKET")
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());

    let client = IPCClient::new(&socket_path);

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <command> [args...]", args[0]);
        println!("Commands:");
        println!("  start                    - Start trading system");
        println!("  stop                     - Stop trading system");
        println!("  emergency-stop           - Emergency stop");
        println!("  status                   - Get system status");
        println!("  account                  - Get account info");
        println!("  positions                - Get positions");
        println!("  orders                   - Get open orders");
        println!("  cancel-all               - Cancel all orders");
        println!("  load-model <path> <ver>  - Load model");
        return Ok(());
    }

    let command = &args[1];

    let response = match command.as_str() {
        "start" => {
            info!("Starting trading system...");
            client.start().await?
        }
        "stop" => {
            info!("Stopping trading system...");
            client.stop().await?
        }
        "emergency-stop" => {
            info!("Emergency stop...");
            client.emergency_stop().await?
        }
        "status" => {
            info!("Getting system status...");
            client.get_status().await?
        }
        "account" => {
            info!("Getting account info...");
            client.get_account().await?
        }
        "positions" => {
            info!("Getting positions...");
            client.get_positions().await?
        }
        "orders" => {
            info!("Getting open orders...");
            client.get_open_orders().await?
        }
        "cancel-all" => {
            info!("Cancelling all orders...");
            client.cancel_all_orders().await?
        }
        "load-model" => {
            if args.len() < 4 {
                error!("load-model requires <path> and <version> arguments");
                return Ok(());
            }
            let path = args[2].clone();
            let version = args[3].clone();
            info!("Loading model {} version {}...", path, version);
            client.load_model(path, version, None).await?
        }
        _ => {
            error!("Unknown command: {}", command);
            return Ok(());
        }
    };

    // Print response
    match response {
        Response::Ok => {
            info!("Command executed successfully");
        }
        Response::Data(data) => {
            info!("Command executed successfully");
            println!("{:#?}", data);
        }
        Response::Error { message, code } => {
            error!("Command failed: {} (code: {:?})", message, code);
        }
    }

    Ok(())
}
// Archived legacy example
