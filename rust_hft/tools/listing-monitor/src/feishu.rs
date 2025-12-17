//! Feishu (Lark) webhook notification module

use anyhow::Result;
use serde_json::json;
use tracing::{debug, error, warn};

const FEISHU_WEBHOOK: &str =
    "https://open.feishu.cn/open-apis/bot/v2/hook/2ad78ad0-0325-48e8-b0ed-d813774c810d";

/// Send an alert to Feishu webhook
pub async fn send_alert(title: &str, content: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let payload = json!({
        "msg_type": "interactive",
        "card": {
            "header": {
                "title": {
                    "tag": "plain_text",
                    "content": title
                },
                "template": "red"
            },
            "elements": [{
                "tag": "markdown",
                "content": content
            }, {
                "tag": "note",
                "elements": [{
                    "tag": "plain_text",
                    "content": format!("Sent at: {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"))
                }]
            }]
        }
    });

    debug!("Sending Feishu alert: {}", title);

    match client.post(FEISHU_WEBHOOK).json(&payload).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                debug!("Feishu alert sent successfully");
                Ok(())
            } else {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                warn!("Feishu webhook returned non-success status: {} - {}", status, body);
                Ok(()) // Don't fail the whole loop for notification errors
            }
        }
        Err(e) => {
            error!("Failed to send Feishu alert: {}", e);
            Err(e.into())
        }
    }
}

/// Send a simple text message to Feishu
pub async fn send_text(text: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let payload = json!({
        "msg_type": "text",
        "content": {
            "text": text
        }
    });

    client.post(FEISHU_WEBHOOK).json(&payload).send().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Run manually: cargo test --package listing-monitor -- --ignored
    async fn test_send_alert() {
        let result = send_alert(
            "Test Alert",
            "This is a test message from listing-monitor",
        )
        .await;
        assert!(result.is_ok());
    }
}
