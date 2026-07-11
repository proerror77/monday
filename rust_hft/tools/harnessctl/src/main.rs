use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
#[command(name = "harnessctl")]
#[command(about = "Loop Engineer Alpha Harness operator helper")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print validation lanes for local development.
    ValidationLanes,
    /// Print the trusted-key JSON entry for a runtime feedback signing key.
    FeedbackPublicKey {
        #[arg(long)]
        signing_key: PathBuf,
        #[arg(long)]
        key_id: String,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.command {
        Command::ValidationLanes => {
            println!("contracts: cargo test -p <changed-research-crate> --locked");
            println!("stores: cargo test -p <changed-store-crate> --locked");
            println!("orchestrator: cargo check -p alpha-harness --locked");
            println!("runtime: cargo check -p hft-live --locked");
        }
        Command::FeedbackPublicKey {
            signing_key,
            key_id,
        } => {
            let encoded = std::fs::read_to_string(&signing_key).map_err(|error| {
                anyhow::anyhow!(
                    "failed to read feedback signing key {}: {error}",
                    signing_key.display()
                )
            })?;
            println!("{}", feedback_public_key_json(&key_id, &encoded)?);
        }
    }
    Ok(())
}

fn feedback_public_key_json(key_id: &str, encoded_private_key: &str) -> anyhow::Result<String> {
    if key_id.trim().is_empty() {
        anyhow::bail!("feedback key id cannot be empty");
    }
    let bytes = hex::decode(encoded_private_key.trim())
        .map_err(|_| anyhow::anyhow!("feedback signing key must be hex encoded"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("feedback signing key must contain exactly 32 bytes"))?;
    let key = SigningKey::from_bytes(&bytes);
    serde_json::to_string_pretty(&BTreeMap::from([(
        key_id.to_string(),
        hex::encode(key.verifying_key().to_bytes()),
    )]))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_feedback_public_key_without_exposing_private_material() {
        let private = hex::encode([7_u8; 32]);
        let output = feedback_public_key_json("runtime-feedback-1", &private).unwrap();
        let parsed: BTreeMap<String, String> = serde_json::from_str(&output).unwrap();
        let expected = SigningKey::from_bytes(&[7_u8; 32]).verifying_key();

        assert_eq!(
            parsed.get("runtime-feedback-1"),
            Some(&hex::encode(expected.to_bytes()))
        );
        assert!(!output.contains(&private));
    }

    #[test]
    fn rejects_invalid_feedback_key_inputs() {
        assert!(feedback_public_key_json("", &hex::encode([7_u8; 32])).is_err());
        assert!(feedback_public_key_json("runtime-feedback-1", "not-hex").is_err());
        assert!(feedback_public_key_json("runtime-feedback-1", "00").is_err());
    }
}
