use crate::fan_control::FanControlPlan;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperRequest {
    Ping,
    ApplyPlan { plan: FanControlPlan },
    ReadStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperResponse {
    pub ok: bool,
    pub message: String,
    pub status: Option<HelperStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperStatus {
    pub version: String,
    pub socket_path: String,
}

impl HelperResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            status: None,
        }
    }

    pub fn ok_with_status(message: impl Into<String>, status: HelperStatus) -> Self {
        Self {
            ok: true,
            message: message.into(),
            status: Some(status),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            status: None,
        }
    }
}
