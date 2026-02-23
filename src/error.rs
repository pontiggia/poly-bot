//! Error types for the Polymarket bot

use thiserror::Error;

/// Main error type for the bot
#[derive(Error, Debug)]
pub enum BotError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing error: {0}")]
    Json(String),

    #[error("Signing error: {0}")]
    Signing(String),

    #[error("Order error: {0}")]
    Order(String),

    #[error("API error: {code} - {message}")]
    Api { code: String, message: String },

    #[error("Kill switch activated")]
    KillSwitch,

    #[error("Circuit breaker open")]
    CircuitBreakerOpen,

    #[error("Insufficient funds: need {needed}, have {available}")]
    InsufficientFunds {
        needed: rust_decimal::Decimal,
        available: rust_decimal::Decimal,
    },

    #[error("Market not found: {0}")]
    MarketNotFound(String),

    #[error("Invalid state transition: {from} -> {to}")]
    InvalidStateTransition { from: String, to: String },

    #[error("Reconciliation error: {0}")]
    Reconciliation(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type alias for bot operations
pub type Result<T> = std::result::Result<T, BotError>;

/// API error types for circuit breaker classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorType {
    /// Temporary errors that can be retried
    Retryable,
    /// Permanent errors that should not be retried
    Fatal,
    /// Expected errors (e.g., FOK not filled) - don't count toward circuit breaker
    Expected,
    /// Critical errors that should immediately trip the circuit breaker
    /// Used for catastrophic failures like orphaned positions
    Critical,
}

impl ErrorType {
    /// Classify an API error message
    pub fn from_error_msg(error_msg: &str) -> Self {
        match error_msg {
            // Retryable - temporary issues
            "ORDER_DELAYED" | "MARKET_NOT_READY" => ErrorType::Retryable,

            // Expected - normal operation, not failures
            "FOK_ORDER_NOT_FILLED_ERROR" => ErrorType::Expected,

            // Fatal - permanent errors
            "INVALID_ORDER_MIN_TICK_SIZE"
            | "INVALID_ORDER_MIN_SIZE"
            | "INVALID_ORDER_DUPLICATED"
            | "INVALID_ORDER_NOT_ENOUGH_BALANCE"
            | "INVALID_ORDER_EXPIRATION"
            | "INVALID_ORDER_ERROR"
            | "EXECUTION_ERROR"
            | "INVALID_SIGNATURE"
            | "NONCE_ALREADY_USED" => ErrorType::Fatal,

            // Unknown errors are treated as fatal
            _ => ErrorType::Fatal,
        }
    }

    /// Should this error count toward circuit breaker threshold?
    pub fn counts_toward_circuit_breaker(&self) -> bool {
        matches!(self, ErrorType::Fatal | ErrorType::Critical)
    }

    /// Is this a critical error that should immediately trip the circuit breaker?
    pub fn is_critical(&self) -> bool {
        matches!(self, ErrorType::Critical)
    }

    /// Can this error be retried?
    pub fn is_retryable(&self) -> bool {
        matches!(self, ErrorType::Retryable)
    }
}

/// Convert from ExchangeError to ErrorType
impl From<&crate::exchange::ExchangeError> for ErrorType {
    fn from(e: &crate::exchange::ExchangeError) -> Self {
        use crate::exchange::ExchangeError;
        match e {
            ExchangeError::AuthenticationFailed(_) => ErrorType::Critical,
            ExchangeError::InvalidParams(_) => ErrorType::Fatal,
            // "not enough balance" is often transient (e.g., settlement delay for neg-risk tokens)
            // rather than a true permanent failure. Retryable prevents circuit breaker tripping.
            ExchangeError::InsufficientBalance(_) => ErrorType::Retryable,
            ExchangeError::Signing(_) => ErrorType::Critical,
            ExchangeError::Configuration(_) => ErrorType::Critical,
            ExchangeError::RateLimited(_) => ErrorType::Retryable,
            ExchangeError::Network(_) => ErrorType::Retryable,
            ExchangeError::OrderRejected { .. } => ErrorType::Fatal,
            ExchangeError::FakKilled(_) => ErrorType::Expected,
            ExchangeError::OrderNotFound(_) => ErrorType::Expected,
            ExchangeError::MarketClosed(_) => ErrorType::Expected,
            ExchangeError::Sdk(_) => ErrorType::Fatal,
        }
    }
}
