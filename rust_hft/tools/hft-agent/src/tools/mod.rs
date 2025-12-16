//! Tool definitions for the HFT Ops Agent
//!
//! Tools follow the Anthropic Tool Use specification.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Tool definition for Anthropic API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Tool call from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Result of tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    #[serde(rename = "type")]
    pub result_type: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ToolResult {
    pub fn success(tool_use_id: String, content: String) -> Self {
        Self {
            tool_use_id,
            result_type: "tool_result".to_string(),
            content,
            is_error: None,
        }
    }

    pub fn error(tool_use_id: String, error: String) -> Self {
        Self {
            tool_use_id,
            result_type: "tool_result".to_string(),
            content: error,
            is_error: Some(true),
        }
    }
}

/// Get all available tools for the HFT Ops Agent
pub fn get_all_tools() -> Vec<Tool> {
    vec![
        // System Status Tools
        get_system_status_tool(),
        get_portfolio_status_tool(),
        health_check_tool(),
        // Trading Control Tools
        pause_trading_tool(),
        resume_trading_tool(),
        enter_degrade_mode_tool(),
        emergency_stop_tool(),
        cancel_all_orders_tool(),
        // Risk Management Tools
        update_risk_config_tool(),
        // Model Management Tools
        load_model_tool(),
        // Alert Tools
        send_alert_tool(),
        log_decision_tool(),
    ]
}

fn get_system_status_tool() -> Tool {
    Tool {
        name: "get_system_status".to_string(),
        description: "Get the current system status including latency metrics, order statistics, \
            WebSocket connection state, and trading mode. Use this to monitor system health."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn get_portfolio_status_tool() -> Tool {
    Tool {
        name: "get_portfolio_status".to_string(),
        description: "Get the current portfolio status including cash balance, equity, PnL, \
            drawdown metrics, and all open positions. Use this to monitor financial health."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn health_check_tool() -> Tool {
    Tool {
        name: "health_check".to_string(),
        description: "Perform a comprehensive health check of all system components. \
            Returns health status for each component (engine, risk, execution, data feed)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn pause_trading_tool() -> Tool {
    Tool {
        name: "pause_trading".to_string(),
        description: "Pause all trading activity. The system will stop sending new orders \
            but will keep monitoring. Use this when you detect anomalies that need investigation."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn resume_trading_tool() -> Tool {
    Tool {
        name: "resume_trading".to_string(),
        description: "Resume trading after a pause. Only use this after confirming that \
            the issue that caused the pause has been resolved."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn enter_degrade_mode_tool() -> Tool {
    Tool {
        name: "enter_degrade_mode".to_string(),
        description: "Enter degraded mode where trading frequency is reduced. \
            Use this when latency is elevated but not critical, or during high volatility."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
    }
}

fn emergency_stop_tool() -> Tool {
    Tool {
        name: "emergency_stop".to_string(),
        description: "EMERGENCY: Stop all trading immediately with optional order cancellation \
            and position closing. Use only in critical situations like: \
            severe drawdown (>7%), system malfunction, or exchange issues."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "The reason for emergency stop"
                },
                "cancel_orders": {
                    "type": "boolean",
                    "description": "Whether to cancel all open orders",
                    "default": true
                },
                "close_positions": {
                    "type": "boolean",
                    "description": "Whether to close all open positions",
                    "default": false
                }
            },
            "required": ["reason"]
        }),
    }
}

fn cancel_all_orders_tool() -> Tool {
    Tool {
        name: "cancel_all_orders".to_string(),
        description: "Cancel all open orders, optionally filtered by symbol or venue. \
            Use this to reduce risk exposure or before stopping trading."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Optional: Only cancel orders for this symbol"
                },
                "venue": {
                    "type": "string",
                    "description": "Optional: Only cancel orders on this venue/exchange"
                }
            },
            "required": []
        }),
    }
}

fn update_risk_config_tool() -> Tool {
    Tool {
        name: "update_risk_config".to_string(),
        description: "Update risk management parameters dynamically. \
            Use this to tighten risk limits during volatile periods or after losses."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "max_drawdown_pct": {
                    "type": "number",
                    "description": "Maximum allowed drawdown percentage (e.g., 5.0 for 5%)"
                },
                "max_position_usd": {
                    "type": "number",
                    "description": "Maximum position size in USD"
                },
                "max_order_size_usd": {
                    "type": "number",
                    "description": "Maximum single order size in USD"
                },
                "latency_threshold_us": {
                    "type": "integer",
                    "description": "Latency threshold in microseconds before degrading"
                },
                "max_orders_per_second": {
                    "type": "integer",
                    "description": "Maximum orders allowed per second"
                }
            },
            "required": []
        }),
    }
}

fn load_model_tool() -> Tool {
    Tool {
        name: "load_model".to_string(),
        description: "Load a new ML model for strategy execution. \
            The model will be downloaded, verified against SHA256 hash, and hot-swapped."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to download the model from (file:// or http://)"
                },
                "sha256": {
                    "type": "string",
                    "description": "Expected SHA256 hash of the model file"
                },
                "version": {
                    "type": "string",
                    "description": "Version identifier for the model"
                },
                "model_type": {
                    "type": "string",
                    "description": "Model type (onnx or pt)",
                    "enum": ["onnx", "pt"]
                }
            },
            "required": ["url", "sha256", "version", "model_type"]
        }),
    }
}

fn send_alert_tool() -> Tool {
    Tool {
        name: "send_alert".to_string(),
        description: "Send an alert notification via configured channels (webhook, email). \
            Use this to notify operators of important events or anomalies."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "severity": {
                    "type": "string",
                    "description": "Alert severity level",
                    "enum": ["info", "warning", "critical"]
                },
                "title": {
                    "type": "string",
                    "description": "Short alert title"
                },
                "message": {
                    "type": "string",
                    "description": "Detailed alert message"
                },
                "context": {
                    "type": "object",
                    "description": "Additional context data"
                }
            },
            "required": ["severity", "title", "message"]
        }),
    }
}

fn log_decision_tool() -> Tool {
    Tool {
        name: "log_decision".to_string(),
        description: "Log a decision made by the agent for audit trail. \
            Always use this after making an important decision."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "decision": {
                    "type": "string",
                    "description": "The decision made"
                },
                "reasoning": {
                    "type": "string",
                    "description": "The reasoning behind the decision"
                },
                "action_taken": {
                    "type": "string",
                    "description": "The action that was taken"
                }
            },
            "required": ["decision", "reasoning", "action_taken"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_tools_have_valid_schema() {
        let tools = get_all_tools();
        assert!(!tools.is_empty());
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert!(tool.input_schema.is_object());
        }
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("id123".to_string(), "success".to_string());
        assert!(result.is_error.is_none());
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("id123".to_string(), "error".to_string());
        assert_eq!(result.is_error, Some(true));
    }
}
