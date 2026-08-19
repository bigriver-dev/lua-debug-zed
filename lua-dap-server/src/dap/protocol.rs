use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level DAP message envelope.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProtocolMessage {
    Request(Request),
    Response(Response),
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub seq: i64,
    #[serde(rename = "type")]
    pub raw_type: String,
    pub request_seq: i64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

impl Response {
    pub fn success(request_seq: i64, command: impl Into<String>, body: Option<Value>) -> Self {
        // https://github.com/microsoft/debug-adapter-protocol/blob/main/specification.md
        Self {
            seq: 0,
            raw_type: "response".to_string(),
            request_seq,
            success: true,
            command: command.into(),
            message: None,
            body,
        }
    }

    pub fn error(request_seq: i64, command: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            seq: 0,
            raw_type: "response".to_string(),
            request_seq,
            success: false,
            command: command.into(),
            message: Some(message.into()),
            body: None,
        }
    }
}

// ============================================================================
// Events
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    #[serde(rename = "type")]
    pub raw_type: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

impl Event {
    pub fn new(event: impl Into<String>, body: Option<Value>) -> Self {
        Self {
            seq: 0,
            raw_type: "event".to_string(),
            event: event.into(),
            body,
        }
    }

    pub fn stopped(reason: &str, thread_id: i64) -> Self {
        Self {
            seq: 0,
            raw_type: "event".to_string(),
            event: "stopped".to_string(),
            body: Some(serde_json::json!({
                "reason": reason,
                "threadId": thread_id,
                "allThreadsStopped": true
            })),
        }
    }

    pub fn initialized() -> Self {
        Self {
            seq: 0,
            raw_type: "event".to_string(),
            event: "initialized".to_string(),
            body: None,
        }
    }

    pub fn terminated() -> Self {
        Self {
            seq: 0,
            raw_type: "event".to_string(),
            event: "terminated".to_string(),
            body: None,
        }
    }
}

// ============================================================================
// Core Protocol Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakpoint {
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsArguments {
    pub source: Source,
    #[serde(default)]
    pub breakpoints: Vec<SourceBreakpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    pub id: Option<usize>,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: usize,
    pub name: String,
    pub source: Option<Source>,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: usize,
    pub expensive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub var_type: Option<String>,
    pub variables_reference: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluateArguments {
    pub expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequestArguments {
    pub program: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c_dll_dir: Option<String>,
    #[serde(default)]
    pub stop_on_entry: bool,
    #[serde(default)]
    pub preload_paths: Vec<String>,
}
