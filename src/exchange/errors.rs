//! Exchange error types with retry classification
//!
//! Errors are classified to help the circuit breaker and retry logic
//! determine appropriate responses to failures.

use thiserror::Error;

/// Exchange operation errors
#[derive(Error, Debug, Clone)]
pub enum ExchangeError {
    /// Authentication failed (credentials invalid or expired)
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Invalid order parameters (price, size, token_id, etc.)
    #[error("Invalid order parameters: {0}")]
    InvalidParams(String),

    /// Order rejected by exchange (e.g., amount mismatch, market closed)
    #[error("Order rejected: {code} - {message}")]
    OrderRejected { code: String, message: String },

    /// Insufficient balance or allowance
    #[error("Insufficient balance: {0}")]
    InsufficientBalance(String),

    /// Rate limited by exchange
    #[error("Rate limited: {0}")]
    RateLimited(String),

    /// Network/connection error
    #[error("Network error: {0}")]
    Network(String),

    /// SDK internal error
    #[error("SDK error: {0}")]
    Sdk(String),

    /// Order not found
    #[error("Order not found: {0}")]
    OrderNotFound(String),

    /// FAK/FOK order killed (no matching liquidity) — not a real error
    #[error("FAK order killed: {0}")]
    FakKilled(String),

    /// Market is closed or not accepting orders
    #[error("Market closed: {0}")]
    MarketClosed(String),

    /// Signing error
    #[error("Signing error: {0}")]
    Signing(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Configuration(String),
}

impl ExchangeError {
    /// Returns true if this error is retryable
    ///
    /// Retryable errors are transient failures that may succeed on retry:
    /// - Rate limiting
    /// - Network errors
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited(_) | Self::Network(_))
    }

    /// Returns true if this error is fatal (should not retry)
    ///
    /// Fatal errors indicate a persistent problem that won't resolve:
    /// - Authentication failures
    /// - Invalid parameters
    /// - Insufficient balance
    /// - Configuration errors
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::AuthenticationFailed(_)
                | Self::InvalidParams(_)
                | Self::InsufficientBalance(_)
                | Self::Configuration(_)
        )
    }

    /// Returns true if this error should trigger the circuit breaker
    ///
    /// Repeated occurrences of these errors suggest a systemic problem
    /// that requires human intervention.
    pub fn triggers_circuit_breaker(&self) -> bool {
        matches!(
            self,
            Self::AuthenticationFailed(_) | Self::InvalidParams(_) | Self::Signing(_)
        )
    }

    /// Create an error from an API error message
    pub fn from_api_error(status: u16, message: &str) -> Self {
        let msg = message.to_lowercase();

        // Classify based on common error patterns
        if msg.contains("invalid amounts") || msg.contains("maker amount") {
            return Self::InvalidParams(message.to_string());
        }

        if msg.contains("insufficient") || msg.contains("balance") {
            return Self::InsufficientBalance(message.to_string());
        }

        if msg.contains("rate limit") || status == 429 {
            return Self::RateLimited(message.to_string());
        }

        if msg.contains("not found") || status == 404 {
            return Self::OrderNotFound(message.to_string());
        }

        if msg.contains("unauthorized") || msg.contains("auth") || status == 401 {
            return Self::AuthenticationFailed(message.to_string());
        }

        if msg.contains("market") && (msg.contains("closed") || msg.contains("not accepting")) {
            return Self::MarketClosed(message.to_string());
        }

        // Default: treat as rejection
        Self::OrderRejected {
            code: status.to_string(),
            message: message.to_string(),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_classification() {
        assert!(ExchangeError::RateLimited("too fast".into()).is_retryable());
        assert!(ExchangeError::Network("timeout".into()).is_retryable());
        assert!(!ExchangeError::InvalidParams("bad price".into()).is_retryable());
    }

    #[test]
    fn test_fatal_classification() {
        assert!(ExchangeError::AuthenticationFailed("bad key".into()).is_fatal());
        assert!(ExchangeError::InvalidParams("bad price".into()).is_fatal());
        assert!(ExchangeError::InsufficientBalance("0".into()).is_fatal());
        assert!(!ExchangeError::Network("timeout".into()).is_fatal());
    }

    #[test]
    fn test_circuit_breaker_trigger() {
        assert!(ExchangeError::AuthenticationFailed("bad".into()).triggers_circuit_breaker());
        assert!(ExchangeError::Signing("failed".into()).triggers_circuit_breaker());
        assert!(!ExchangeError::Network("timeout".into()).triggers_circuit_breaker());
    }

    #[test]
    fn test_from_api_error() {
        // Invalid amounts pattern
        let e = ExchangeError::from_api_error(400, "invalid amounts, the maker amount...");
        assert!(matches!(e, ExchangeError::InvalidParams(_)));

        // Rate limit
        let e = ExchangeError::from_api_error(429, "rate limit exceeded");
        assert!(matches!(e, ExchangeError::RateLimited(_)));

        // Auth
        let e = ExchangeError::from_api_error(401, "unauthorized");
        assert!(matches!(e, ExchangeError::AuthenticationFailed(_)));
    }
}
