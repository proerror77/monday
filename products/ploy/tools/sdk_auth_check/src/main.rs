use std::str::FromStr;

use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use polymarket_client_sdk::clob::types::{request::BalanceAllowanceRequest, AssetType};
use polymarket_client_sdk::clob::{Client, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Intentionally minimal: validate that the upstream SDK can authenticate
    // in this environment without triggering the SDK Synchronization guard.
    let private_key_hex = std::env::var("POLYMARKET_PRIVATE_KEY")
        .or_else(|_| std::env::var("PRIVATE_KEY"))
        .expect("POLYMARKET_PRIVATE_KEY or PRIVATE_KEY must be set");

    let signer = PrivateKeySigner::from_str(private_key_hex.trim_start_matches("0x"))?
        .with_chain_id(Some(polymarket_client_sdk::POLYGON));

    let client = Client::new("https://clob.polymarket.com", Config::default())?
        .authentication_builder(&signer)
        .authenticate()
        .await?;

    let req = BalanceAllowanceRequest::builder()
        .asset_type(AssetType::Collateral)
        .build();
    let bal = client.balance_allowance(req).await?;

    println!("sdk auth ok: {}", client.address());
    println!("sdk usdc balance: {}", bal.balance);
    Ok(())
}
