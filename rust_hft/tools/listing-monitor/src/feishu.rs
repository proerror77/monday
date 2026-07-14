//! Feishu (Lark) webhook notification module.

use anyhow::{bail, Context, Result};
use serde_json::json;
use tracing::{debug, error, warn};

const FEISHU_WEBHOOK_ENV: &str = "FEISHU_WEBHOOK_URL";

pub fn validate_config() -> Result<()> {
    webhook_url().map(|_| ())
}

fn webhook_url() -> Result<reqwest::Url> {
    parse_webhook_url(std::env::var(FEISHU_WEBHOOK_ENV).ok().as_deref())
}

fn parse_webhook_url(value: Option<&str>) -> Result<reqwest::Url> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| {
            format!("required environment variable {FEISHU_WEBHOOK_ENV} is missing")
        })?;
    let url = reqwest::Url::parse(value)
        .with_context(|| format!("{FEISHU_WEBHOOK_ENV} is not a valid URL"))?;
    if url.scheme() != "https" {
        bail!("{FEISHU_WEBHOOK_ENV} must use HTTPS");
    }
    if !matches!(
        url.host_str(),
        Some("open.feishu.cn" | "open.larksuite.com")
    ) {
        bail!("{FEISHU_WEBHOOK_ENV} must use an official Feishu or Lark host");
    }
    if !url.path().starts_with("/open-apis/bot/v2/hook/") {
        bail!("{FEISHU_WEBHOOK_ENV} must be a bot webhook URL");
    }
    Ok(url)
}

/// Send an alert to Feishu webhook
pub async fn send_alert(title: &str, content: &str) -> Result<()> {
    let webhook_url = webhook_url()?;
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

    match client.post(webhook_url).json(&payload).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                debug!("Feishu alert sent successfully");
                Ok(())
            } else {
                let status = resp.status();
                warn!("Feishu webhook returned non-success status: {}", status);
                Ok(()) // Don't fail the whole loop for notification errors
            }
        }
        Err(e) => {
            let e = e.without_url();
            error!("Failed to send Feishu alert: {}", e);
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_is_required_and_must_be_official_https() {
        assert!(parse_webhook_url(None).is_err());
        assert!(parse_webhook_url(Some(" ")).is_err());
        assert!(
            parse_webhook_url(Some("http://open.feishu.cn/open-apis/bot/v2/hook/test")).is_err()
        );
        assert!(parse_webhook_url(Some("https://example.com/hook/test")).is_err());
        assert!(parse_webhook_url(Some("https://open.feishu.cn/other/path")).is_err());
    }

    #[test]
    fn accepts_feishu_and_lark_webhook_hosts() {
        assert!(
            parse_webhook_url(Some("https://open.feishu.cn/open-apis/bot/v2/hook/test")).is_ok()
        );
        assert!(parse_webhook_url(Some(
            "https://open.larksuite.com/open-apis/bot/v2/hook/test"
        ))
        .is_ok());
    }
}
