use chrono::Utc;
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    RequestUnauthorized,
    RequestHeadersTooLarge,
    RequestBodyTooLarge,
    RequestInvalid,
    RelayBusy,
    RelayShuttingDown,
    BackendConnectFailed,
    BackendExited,
    BackendRestartFailed,
    ProviderAuthFailed,
    ProviderRateLimited,
    UpstreamServerError,
    FirstByteTimeout,
    MidstreamIdleTimeout,
    ClientCancelled,
    ModelFallback,
    ContextWindowExceeded,
    LoginFailed,
    LoginCancelled,
    ClaudeExited,
    ConfigTransactionFailed,
    AccountAutoFailover,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestUnauthorized => "BAS-RELAY-001",
            Self::RequestHeadersTooLarge => "BAS-RELAY-002",
            Self::RequestBodyTooLarge => "BAS-RELAY-003",
            Self::RequestInvalid => "BAS-RELAY-004",
            Self::RelayBusy => "BAS-RELAY-005",
            Self::RelayShuttingDown => "BAS-RELAY-006",
            Self::BackendConnectFailed => "BAS-BACKEND-001",
            Self::BackendExited => "BAS-BACKEND-002",
            Self::BackendRestartFailed => "BAS-BACKEND-003",
            Self::ProviderAuthFailed => "BAS-UPSTREAM-001",
            Self::ProviderRateLimited => "BAS-UPSTREAM-002",
            Self::UpstreamServerError => "BAS-UPSTREAM-003",
            Self::FirstByteTimeout => "BAS-UPSTREAM-004",
            Self::MidstreamIdleTimeout => "BAS-UPSTREAM-005",
            Self::ClientCancelled => "BAS-CLIENT-001",
            Self::ModelFallback => "BAS-ROUTE-001",
            Self::ContextWindowExceeded => "BAS-ROUTE-002",
            Self::LoginFailed => "BAS-AUTH-001",
            Self::LoginCancelled => "BAS-AUTH-002",
            Self::ClaudeExited => "BAS-CLAUDE-001",
            Self::ConfigTransactionFailed => "BAS-CONFIG-001",
            Self::AccountAutoFailover => "BAS-ACCOUNT-001",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvent {
    pub timestamp: String,
    pub correlation_id: Option<String>,
    pub code: String,
    pub severity: String,
    pub message: String,
    pub http_status: Option<u16>,
    pub provider: Option<String>,
}

static EVENTS: OnceLock<Mutex<VecDeque<DiagnosticEvent>>> = OnceLock::new();

fn events() -> &'static Mutex<VecDeque<DiagnosticEvent>> {
    EVENTS.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_EVENTS)))
}

pub fn record(
    code: ErrorCode,
    severity: &str,
    message: &str,
    correlation_id: Option<&str>,
    http_status: Option<u16>,
    provider: Option<&str>,
) {
    let event = DiagnosticEvent {
        timestamp: Utc::now().to_rfc3339(),
        correlation_id: correlation_id.map(str::to_owned),
        code: code.as_str().to_owned(),
        severity: severity.to_owned(),
        // Callers must use fixed, redacted summaries. Limit the retained text as a
        // second line of defense against accidentally retaining an upstream body.
        message: message.chars().take(240).collect(),
        http_status,
        provider: provider.map(str::to_owned),
    };
    if let Ok(mut queue) = events().lock() {
        if queue.len() == MAX_EVENTS {
            queue.pop_front();
        }
        queue.push_back(event);
    }
}

pub fn snapshot() -> Vec<DiagnosticEvent> {
    events()
        .lock()
        .map(|queue| queue.iter().rev().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_cover_expected_failure_boundaries() {
        assert_eq!(ErrorCode::RelayBusy.as_str(), "BAS-RELAY-005");
        assert_eq!(ErrorCode::BackendExited.as_str(), "BAS-BACKEND-002");
        assert_eq!(ErrorCode::ProviderRateLimited.as_str(), "BAS-UPSTREAM-002");
        assert_eq!(ErrorCode::ClientCancelled.as_str(), "BAS-CLIENT-001");
        assert_eq!(ErrorCode::ContextWindowExceeded.as_str(), "BAS-ROUTE-002");
    }

    #[test]
    fn diagnostics_are_bounded_and_never_store_more_than_the_summary_limit() {
        let long = "x".repeat(400);
        record(
            ErrorCode::RequestInvalid,
            "error",
            &long,
            None,
            Some(400),
            None,
        );
        let latest = snapshot().into_iter().next().unwrap();
        assert_eq!(latest.message.len(), 240);
    }
}
