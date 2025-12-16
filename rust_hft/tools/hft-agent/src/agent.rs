//! HFT Ops Agent - LLM-powered autonomous operations agent

use crate::config::AgentConfig;
use crate::grpc_client::{
    format_health_response, format_portfolio_status, format_system_status, HftClient,
};
use crate::tools::{get_all_tools, Tool, ToolResult, ToolUse};
use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const ANTHROPIC_MESSAGES_API: &str = "/v1/messages";

/// Main HFT Ops Agent
pub struct HftOpsAgent {
    config: AgentConfig,
    http_client: Client,
    grpc_client: Arc<Mutex<HftClient>>,
    tools: Vec<Tool>,
    system_prompt: String,
    alert_cooldowns: HashMap<String, Instant>,
    decision_log: Vec<DecisionLogEntry>,
}

/// Decision log entry for audit trail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionLogEntry {
    pub timestamp: String,
    pub decision: String,
    pub reasoning: String,
    pub action_taken: String,
    pub context: Option<Value>,
}

/// Anthropic API request
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    tools: Vec<Tool>,
    messages: Vec<Message>,
}

/// Anthropic API response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AnthropicResponse {
    id: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
enum Message {
    #[serde(rename = "user")]
    User { content: Vec<ContentBlock> },
    #[serde(rename = "assistant")]
    Assistant { content: Vec<ContentBlock> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

impl HftOpsAgent {
    /// Create a new HFT Ops Agent
    pub async fn new(config: AgentConfig) -> Result<Self> {
        let api_key = config
            .get_api_key()
            .context("ANTHROPIC_API_KEY not set in config or environment")?;

        let http_client = Client::builder()
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(
                    "x-api-key",
                    api_key.parse().context("Invalid API key format")?,
                );
                headers.insert(
                    "anthropic-version",
                    "2023-06-01".parse().unwrap(),
                );
                headers.insert("content-type", "application/json".parse().unwrap());
                headers
            })
            .build()
            .context("Failed to create HTTP client")?;

        let grpc_client = HftClient::connect(&config.grpc)
            .await
            .context("Failed to connect to gRPC server")?;

        let tools = get_all_tools();
        let system_prompt = build_system_prompt(&config);

        Ok(Self {
            config,
            http_client,
            grpc_client: Arc::new(Mutex::new(grpc_client)),
            tools,
            system_prompt,
            alert_cooldowns: HashMap::new(),
            decision_log: Vec::new(),
        })
    }

    /// Run the agent monitoring loop
    pub async fn run(&mut self) -> Result<()> {
        info!("Starting HFT Ops Agent monitoring loop");

        loop {
            match self.monitoring_cycle().await {
                Ok(_) => {
                    debug!("Monitoring cycle completed successfully");
                }
                Err(e) => {
                    error!(error = %e, "Monitoring cycle failed");
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(
                self.config.agent.monitor_interval_secs,
            ))
            .await;
        }
    }

    /// Single monitoring cycle
    async fn monitoring_cycle(&mut self) -> Result<()> {
        // Collect system status
        let status = {
            let mut client = self.grpc_client.lock().await;
            client.get_status().await?
        };

        let portfolio = {
            let mut client = self.grpc_client.lock().await;
            client.get_portfolio_status().await?
        };

        let health = {
            let mut client = self.grpc_client.lock().await;
            client.health_check().await?
        };

        // Build context for LLM
        let context = format!(
            "Current Time: {}\n\n{}\n\n{}\n\n{}",
            Utc::now().to_rfc3339(),
            format_system_status(&status),
            format_portfolio_status(&portfolio),
            format_health_response(&health)
        );

        // Check for anomalies and let LLM decide
        let anomalies = self.detect_anomalies(&status, &portfolio, &health);

        if !anomalies.is_empty() || !self.config.agent.auto_handle_anomalies {
            let prompt = if anomalies.is_empty() {
                format!(
                    "Here is the current system status. Analyze it and determine if any action is needed.\n\n{}",
                    context
                )
            } else {
                format!(
                    "ANOMALIES DETECTED:\n{}\n\nSystem Status:\n{}\n\nAnalyze these anomalies and take appropriate action.",
                    anomalies.join("\n"),
                    context
                )
            };

            self.process_with_llm(&prompt).await?;
        }

        Ok(())
    }

    /// Detect anomalies based on thresholds
    fn detect_anomalies(
        &self,
        status: &crate::grpc_client::proto::SystemStatus,
        portfolio: &crate::grpc_client::proto::PortfolioStatus,
        health: &crate::grpc_client::proto::HealthResponse,
    ) -> Vec<String> {
        let mut anomalies = Vec::new();
        let thresholds = &self.config.agent.thresholds;

        // Check latency
        if status.latency_p99_us as u64 > thresholds.latency_p99_us {
            anomalies.push(format!(
                "HIGH LATENCY: p99={}us exceeds threshold {}us",
                status.latency_p99_us, thresholds.latency_p99_us
            ));
        }

        // Check drawdown
        if portfolio.drawdown_pct > thresholds.drawdown_critical_pct {
            anomalies.push(format!(
                "CRITICAL DRAWDOWN: {:.2}% exceeds critical threshold {:.2}%",
                portfolio.drawdown_pct, thresholds.drawdown_critical_pct
            ));
        } else if portfolio.drawdown_pct > thresholds.drawdown_warn_pct {
            anomalies.push(format!(
                "WARNING DRAWDOWN: {:.2}% exceeds warning threshold {:.2}%",
                portfolio.drawdown_pct, thresholds.drawdown_warn_pct
            ));
        }

        // Check WebSocket reconnects
        if status.ws_reconnect_count as u32 > thresholds.max_ws_reconnects {
            anomalies.push(format!(
                "HIGH RECONNECT COUNT: {} exceeds threshold {}",
                status.ws_reconnect_count, thresholds.max_ws_reconnects
            ));
        }

        // Check order rejection rate
        let total_orders = status.orders_submitted.max(1) as f64;
        let rejection_rate = (status.orders_rejected as f64 / total_orders) * 100.0;
        if rejection_rate > thresholds.order_rejection_rate_pct {
            anomalies.push(format!(
                "HIGH REJECTION RATE: {:.2}% exceeds threshold {:.2}%",
                rejection_rate, thresholds.order_rejection_rate_pct
            ));
        }

        // Check component health
        if !health.healthy {
            for (name, component) in &health.components {
                if !component.healthy {
                    anomalies.push(format!("UNHEALTHY COMPONENT: {} - {}", name, component.message));
                }
            }
        }

        // Check if system is not running
        if !status.is_running {
            anomalies.push("SYSTEM NOT RUNNING".to_string());
        }

        anomalies
    }

    /// Process a prompt with the LLM and execute any tool calls
    async fn process_with_llm(&mut self, user_prompt: &str) -> Result<()> {
        let mut messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: user_prompt.to_string(),
            }],
        }];

        loop {
            let request = AnthropicRequest {
                model: self.config.anthropic.model.clone(),
                max_tokens: self.config.anthropic.max_tokens,
                system: self.system_prompt.clone(),
                tools: self.tools.clone(),
                messages: messages.clone(),
            };

            let url = format!(
                "{}{}",
                self.config.anthropic.api_url, ANTHROPIC_MESSAGES_API
            );

            debug!(url = %url, "Sending request to Anthropic API");

            let response = self
                .http_client
                .post(&url)
                .json(&request)
                .send()
                .await
                .context("Failed to send request to Anthropic API")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("Anthropic API error: {} - {}", status, body);
            }

            let api_response: AnthropicResponse = response
                .json()
                .await
                .context("Failed to parse Anthropic API response")?;

            if let Some(usage) = &api_response.usage {
                debug!(
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    "Token usage"
                );
            }

            // Process response content
            let mut tool_uses = Vec::new();
            for block in &api_response.content {
                match block {
                    ContentBlock::Text { text } => {
                        info!(response = %text, "LLM response");
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_uses.push(ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    _ => {}
                }
            }

            // If no tool uses, we're done
            if tool_uses.is_empty() {
                break;
            }

            // Add assistant message with tool uses
            messages.push(Message::Assistant {
                content: api_response.content.clone(),
            });

            // Execute tools and collect results
            let mut tool_results = Vec::new();
            for tool_use in tool_uses {
                let result = self.execute_tool(&tool_use).await;
                tool_results.push(result);
            }

            // Add tool results as user message
            messages.push(Message::User {
                content: tool_results
                    .into_iter()
                    .map(|r| ContentBlock::ToolResult {
                        tool_use_id: r.tool_use_id,
                        content: r.content,
                        is_error: r.is_error,
                    })
                    .collect(),
            });

            // Check stop reason
            if api_response.stop_reason.as_deref() == Some("end_turn") {
                break;
            }
        }

        Ok(())
    }

    /// Execute a tool call
    async fn execute_tool(&mut self, tool_use: &ToolUse) -> ToolResult {
        info!(tool = %tool_use.name, "Executing tool");

        if self.config.agent.dry_run {
            return ToolResult::success(
                tool_use.id.clone(),
                format!("[DRY RUN] Would execute: {} with {:?}", tool_use.name, tool_use.input),
            );
        }

        match tool_use.name.as_str() {
            "get_system_status" => self.tool_get_system_status(&tool_use.id).await,
            "get_portfolio_status" => self.tool_get_portfolio_status(&tool_use.id).await,
            "health_check" => self.tool_health_check(&tool_use.id).await,
            "pause_trading" => self.tool_pause_trading(&tool_use.id).await,
            "resume_trading" => self.tool_resume_trading(&tool_use.id).await,
            "enter_degrade_mode" => self.tool_enter_degrade_mode(&tool_use.id).await,
            "emergency_stop" => self.tool_emergency_stop(&tool_use.id, &tool_use.input).await,
            "cancel_all_orders" => {
                self.tool_cancel_all_orders(&tool_use.id, &tool_use.input)
                    .await
            }
            "update_risk_config" => {
                self.tool_update_risk_config(&tool_use.id, &tool_use.input)
                    .await
            }
            "load_model" => self.tool_load_model(&tool_use.id, &tool_use.input).await,
            "send_alert" => self.tool_send_alert(&tool_use.id, &tool_use.input).await,
            "log_decision" => self.tool_log_decision(&tool_use.id, &tool_use.input).await,
            _ => ToolResult::error(tool_use.id.clone(), format!("Unknown tool: {}", tool_use.name)),
        }
    }

    // Tool implementations

    async fn tool_get_system_status(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.get_status().await {
            Ok(status) => ToolResult::success(tool_use_id.to_string(), format_system_status(&status)),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_get_portfolio_status(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.get_portfolio_status().await {
            Ok(status) => {
                ToolResult::success(tool_use_id.to_string(), format_portfolio_status(&status))
            }
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_health_check(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.health_check().await {
            Ok(health) => {
                ToolResult::success(tool_use_id.to_string(), format_health_response(&health))
            }
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_pause_trading(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.pause_trading().await {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_resume_trading(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.resume_trading().await {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_enter_degrade_mode(&self, tool_use_id: &str) -> ToolResult {
        match self.grpc_client.lock().await.enter_degrade_mode().await {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_emergency_stop(&self, tool_use_id: &str, input: &Value) -> ToolResult {
        let reason = input["reason"].as_str().unwrap_or("Unknown").to_string();
        let cancel_orders = input["cancel_orders"].as_bool().unwrap_or(true);
        let close_positions = input["close_positions"].as_bool().unwrap_or(false);

        match self
            .grpc_client
            .lock()
            .await
            .emergency_stop(reason, cancel_orders, close_positions)
            .await
        {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_cancel_all_orders(&self, tool_use_id: &str, input: &Value) -> ToolResult {
        let symbol = input["symbol"].as_str().map(String::from);
        let venue = input["venue"].as_str().map(String::from);

        match self
            .grpc_client
            .lock()
            .await
            .cancel_all_orders(symbol, venue)
            .await
        {
            Ok(response) => {
                let msg = format!(
                    "Canceled {} orders, {} failed",
                    response.orders_canceled, response.orders_failed
                );
                ToolResult::success(tool_use_id.to_string(), msg)
            }
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_update_risk_config(&self, tool_use_id: &str, input: &Value) -> ToolResult {
        let max_drawdown_pct = input["max_drawdown_pct"].as_f64();
        let max_position_usd = input["max_position_usd"].as_f64();
        let max_order_size_usd = input["max_order_size_usd"].as_f64();
        let latency_threshold_us = input["latency_threshold_us"].as_i64();
        let max_orders_per_second = input["max_orders_per_second"].as_i64().map(|v| v as i32);

        match self
            .grpc_client
            .lock()
            .await
            .update_risk_config(
                max_drawdown_pct,
                max_position_usd,
                max_order_size_usd,
                latency_threshold_us,
                max_orders_per_second,
            )
            .await
        {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_load_model(&self, tool_use_id: &str, input: &Value) -> ToolResult {
        let url = match input["url"].as_str() {
            Some(u) => u.to_string(),
            None => return ToolResult::error(tool_use_id.to_string(), "Missing required field: url".to_string()),
        };
        let sha256 = match input["sha256"].as_str() {
            Some(s) => s.to_string(),
            None => return ToolResult::error(tool_use_id.to_string(), "Missing required field: sha256".to_string()),
        };
        let version = match input["version"].as_str() {
            Some(v) => v.to_string(),
            None => return ToolResult::error(tool_use_id.to_string(), "Missing required field: version".to_string()),
        };
        let model_type = match input["model_type"].as_str() {
            Some(t) => t.to_string(),
            None => return ToolResult::error(tool_use_id.to_string(), "Missing required field: model_type".to_string()),
        };

        match self
            .grpc_client
            .lock()
            .await
            .load_model(url, sha256, version, model_type)
            .await
        {
            Ok(msg) => ToolResult::success(tool_use_id.to_string(), msg),
            Err(e) => ToolResult::error(tool_use_id.to_string(), format!("Error: {}", e)),
        }
    }

    async fn tool_send_alert(&mut self, tool_use_id: &str, input: &Value) -> ToolResult {
        let severity = input["severity"].as_str().unwrap_or("info");
        let title = input["title"].as_str().unwrap_or("Alert");
        let message = input["message"].as_str().unwrap_or("");

        // Check cooldown
        let alert_key = format!("{}:{}", severity, title);
        if let Some(last_alert) = self.alert_cooldowns.get(&alert_key) {
            if last_alert.elapsed().as_secs() < self.config.alerts.cooldown_secs {
                return ToolResult::success(
                    tool_use_id.to_string(),
                    format!("Alert suppressed (cooldown active): {}", title),
                );
            }
        }

        // Log the alert
        info!(
            severity = severity,
            title = title,
            message = message,
            "Alert triggered"
        );

        // Send to webhook if configured
        if let Some(webhook_url) = &self.config.alerts.webhook_url {
            let payload = json!({
                "severity": severity,
                "title": title,
                "message": message,
                "timestamp": Utc::now().to_rfc3339(),
                "source": "HFT Ops Agent"
            });

            match self.http_client.post(webhook_url).json(&payload).send().await {
                Ok(resp) if resp.status().is_success() => {
                    debug!("Alert sent to webhook successfully");
                }
                Ok(resp) => {
                    warn!(status = %resp.status(), "Failed to send alert to webhook");
                }
                Err(e) => {
                    warn!(error = %e, "Error sending alert to webhook");
                }
            }
        }

        // Update cooldown
        self.alert_cooldowns.insert(alert_key, Instant::now());

        ToolResult::success(
            tool_use_id.to_string(),
            format!("Alert sent: [{}] {}", severity.to_uppercase(), title),
        )
    }

    async fn tool_log_decision(&mut self, tool_use_id: &str, input: &Value) -> ToolResult {
        let decision = input["decision"].as_str().unwrap_or("Unknown decision");
        let reasoning = input["reasoning"].as_str().unwrap_or("No reasoning provided");
        let action_taken = input["action_taken"].as_str().unwrap_or("No action specified");

        let entry = DecisionLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            decision: decision.to_string(),
            reasoning: reasoning.to_string(),
            action_taken: action_taken.to_string(),
            context: input.get("context").cloned(),
        };

        info!(
            decision = %entry.decision,
            reasoning = %entry.reasoning,
            action = %entry.action_taken,
            "Decision logged"
        );

        self.decision_log.push(entry);

        ToolResult::success(
            tool_use_id.to_string(),
            format!("Decision logged: {}", decision),
        )
    }

    /// Get the decision log
    pub fn get_decision_log(&self) -> &[DecisionLogEntry] {
        &self.decision_log
    }
}

/// Build the system prompt for the agent
fn build_system_prompt(config: &AgentConfig) -> String {
    format!(
        r#"You are an HFT (High-Frequency Trading) Operations Agent. Your role is to autonomously monitor and manage a high-frequency trading system.

## Your Responsibilities:
1. Monitor system health (latency, WebSocket connections, component status)
2. Monitor portfolio health (PnL, drawdown, position sizes)
3. Detect and respond to anomalies automatically
4. Make risk management decisions
5. Deploy new models when approved
6. Send alerts for important events
7. Log all decisions for audit trail

## Decision Guidelines:

### Latency Issues:
- p99 > {}us: Enter DEGRADE mode
- p99 > 50000us: PAUSE trading, send CRITICAL alert
- Consistent high latency: Investigate data feed issues

### Drawdown Thresholds:
- > {:.1}%: Send WARNING alert, consider reducing positions
- > {:.1}%: Enter DEGRADE mode, tighten risk limits
- > 7%: EMERGENCY STOP, close all positions

### Connection Issues:
- > {} reconnects: Send WARNING alert
- Data feed down: PAUSE trading immediately
- Multiple components unhealthy: PAUSE trading, investigate

### Order Rejection Rate:
- > {:.1}%: Investigate, may indicate exchange issues

## Action Hierarchy:
1. CONTINUE - Normal operations, log monitoring results
2. WARN - Send alert but continue trading
3. DEGRADE - Reduce trading frequency, tighten risk
4. PAUSE - Stop new orders, keep monitoring
5. EMERGENCY STOP - Cancel all orders, optionally close positions

## Important Rules:
- Always log your reasoning before taking action using log_decision
- Never resume trading without confirming the issue is resolved
- When in doubt, err on the side of caution (pause rather than continue)
- Send alerts for any significant action taken
- Do not close positions unless drawdown exceeds 7% or explicitly requested

## Current Configuration:
- Monitoring interval: {} seconds
- Auto-handle anomalies: {}
- Dry run mode: {}
"#,
        config.agent.thresholds.latency_p99_us,
        config.agent.thresholds.drawdown_warn_pct,
        config.agent.thresholds.drawdown_critical_pct,
        config.agent.thresholds.max_ws_reconnects,
        config.agent.thresholds.order_rejection_rate_pct,
        config.agent.monitor_interval_secs,
        config.agent.auto_handle_anomalies,
        config.agent.dry_run
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    #[test]
    fn test_build_system_prompt() {
        let config = AgentConfig::default();
        let prompt = build_system_prompt(&config);
        assert!(prompt.contains("HFT"));
        assert!(prompt.contains("EMERGENCY STOP"));
    }

    #[test]
    fn test_decision_log_entry_serialization() {
        let entry = DecisionLogEntry {
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            decision: "Pause trading".to_string(),
            reasoning: "High latency detected".to_string(),
            action_taken: "Paused trading".to_string(),
            context: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("Pause trading"));
    }
}
