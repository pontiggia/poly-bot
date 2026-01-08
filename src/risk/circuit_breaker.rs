//! Circuit Breaker - Trading halt on repeated failures
//!
//! Implements a circuit breaker pattern to prevent trading during system instability.
//! Only Fatal errors count toward the threshold (Retryable and Expected are ignored).

use crate::constants::*;
use crate::error::ErrorType;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation - trading allowed
    Closed,
    /// Circuit open - trading halted
    Open,
    /// Testing recovery - limited trading
    HalfOpen,
}

/// Reason the circuit was opened
#[derive(Debug, Clone, PartialEq)]
pub enum OpenReason {
    /// Too many fatal errors
    FatalErrorThreshold { count: u32 },
    /// Reject rate exceeded threshold
    RejectRateExceeded { rate: f64 },
    /// WebSocket disconnected
    WebSocketDisconnect,
    /// Reconciliation found discrepancies
    ReconciliationFailure { details: String },
    /// Manual trigger (kill switch or API)
    ManualTrigger,
    /// Daily loss limit exceeded
    DailyLossExceeded,
    /// Orphaned position detected (failed to cancel arb leg)
    OrphanedPosition { order_id: String },
}

impl std::fmt::Display for OpenReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenReason::FatalErrorThreshold { count } => {
                write!(f, "Fatal error threshold exceeded ({} errors)", count)
            }
            OpenReason::RejectRateExceeded { rate } => {
                write!(f, "Reject rate exceeded ({:.1}%)", rate * 100.0)
            }
            OpenReason::WebSocketDisconnect => write!(f, "WebSocket disconnected"),
            OpenReason::ReconciliationFailure { details } => {
                write!(f, "Reconciliation failure: {}", details)
            }
            OpenReason::ManualTrigger => write!(f, "Manual trigger"),
            OpenReason::DailyLossExceeded => write!(f, "Daily loss limit exceeded"),
            OpenReason::OrphanedPosition { order_id } => {
                write!(f, "Orphaned position detected (order: {})", order_id)
            }
        }
    }
}

/// Circuit breaker for trading safety
pub struct CircuitBreaker {
    /// Current state
    state: RwLock<CircuitState>,
    /// Reason circuit was opened (if open)
    open_reason: RwLock<Option<OpenReason>>,
    /// Time when circuit was opened
    opened_at: RwLock<Option<Instant>>,
    /// Fatal error count in current window
    fatal_error_count: AtomicU32,
    /// Total orders submitted in current window
    orders_submitted: AtomicU32,
    /// Total orders rejected in current window
    orders_rejected: AtomicU32,
    /// Timeout before trying half-open
    reset_timeout: Duration,
    /// Fatal error threshold
    fatal_threshold: u32,
    /// Reject rate threshold (0.0 - 1.0)
    reject_rate_threshold: f64,
    /// Successful orders in half-open state needed to close
    half_open_success_required: u32,
    /// Successful orders in half-open state
    half_open_successes: AtomicU32,
    /// Window start time for rate calculations
    window_start: RwLock<Instant>,
    /// Window duration for rate calculations
    window_duration: Duration,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default settings
    pub fn new() -> Self {
        Self {
            state: RwLock::new(CircuitState::Closed),
            open_reason: RwLock::new(None),
            opened_at: RwLock::new(None),
            fatal_error_count: AtomicU32::new(0),
            orders_submitted: AtomicU32::new(0),
            orders_rejected: AtomicU32::new(0),
            reset_timeout: CIRCUIT_RESET_TIMEOUT,
            fatal_threshold: CIRCUIT_BREAKER_THRESHOLD,
            reject_rate_threshold: REJECT_RATE_THRESHOLD,
            half_open_success_required: 3,
            half_open_successes: AtomicU32::new(0),
            window_start: RwLock::new(Instant::now()),
            window_duration: Duration::from_secs(60),
        }
    }

    /// Create with custom thresholds (for testing)
    pub fn with_thresholds(
        fatal_threshold: u32,
        reject_rate_threshold: f64,
        reset_timeout: Duration,
    ) -> Self {
        Self {
            fatal_threshold,
            reject_rate_threshold,
            reset_timeout,
            ..Self::new()
        }
    }

    /// Check if trading is allowed
    pub fn is_trading_allowed(&self) -> bool {
        self.maybe_transition_to_half_open();

        let state = self.state.read().unwrap();
        matches!(*state, CircuitState::Closed | CircuitState::HalfOpen)
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap()
    }

    /// Get reason circuit was opened
    pub fn open_reason(&self) -> Option<OpenReason> {
        self.open_reason.read().unwrap().clone()
    }

    /// Record an order submission
    pub fn record_order_submitted(&self) {
        self.maybe_reset_window();
        self.orders_submitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an order result (success or failure with error type)
    pub fn record_order_result(&self, error: Option<ErrorType>) {
        self.maybe_reset_window();

        match error {
            None => {
                // Success
                let state = *self.state.read().unwrap();
                if state == CircuitState::HalfOpen {
                    let successes = self.half_open_successes.fetch_add(1, Ordering::Relaxed) + 1;
                    if successes >= self.half_open_success_required {
                        self.close();
                    }
                }
            }
            Some(ErrorType::Fatal) => {
                self.orders_rejected.fetch_add(1, Ordering::Relaxed);
                let count = self.fatal_error_count.fetch_add(1, Ordering::Relaxed) + 1;

                if count >= self.fatal_threshold {
                    self.open(OpenReason::FatalErrorThreshold { count });
                } else {
                    // Check reject rate
                    self.check_reject_rate();
                }
            }
            Some(ErrorType::Retryable) => {
                // Don't count toward threshold
            }
            Some(ErrorType::Expected) => {
                // Don't count toward threshold (e.g., FOK not filled)
            }
            Some(ErrorType::Critical) => {
                // Critical error - immediately trip the circuit breaker
                error!("Circuit breaker: Critical error detected, immediately tripping");
                self.orders_rejected.fetch_add(1, Ordering::Relaxed);
                self.open(OpenReason::OrphanedPosition { 
                    order_id: "unknown".to_string() 
                });
            }
        }
    }

    /// Record WebSocket disconnect
    pub fn record_ws_disconnect(&self) {
        warn!("Circuit breaker: WebSocket disconnected");
        self.open(OpenReason::WebSocketDisconnect);
    }

    /// Record WebSocket reconnect
    pub fn record_ws_reconnect(&self) {
        let state = *self.state.read().unwrap();
        if state == CircuitState::Open {
            if let Some(reason) = self.open_reason() {
                if matches!(reason, OpenReason::WebSocketDisconnect) {
                    info!("Circuit breaker: WebSocket reconnected, transitioning to half-open");
                    self.transition_to_half_open();
                }
            }
        }
    }

    /// Record reconciliation failure
    pub fn record_reconciliation_failure(&self, details: String) {
        error!("Circuit breaker: Reconciliation failure - {}", details);
        self.open(OpenReason::ReconciliationFailure { details });
    }

    /// Record daily loss limit exceeded
    pub fn record_daily_loss_exceeded(&self) {
        error!("Circuit breaker: Daily loss limit exceeded");
        self.open(OpenReason::DailyLossExceeded);
    }

    /// Manually open the circuit
    pub fn manual_open(&self) {
        warn!("Circuit breaker: Manual trigger");
        self.open(OpenReason::ManualTrigger);
    }

    /// Force close the circuit (for testing/recovery)
    pub fn force_close(&self) {
        info!("Circuit breaker: Force closed");
        self.close();
    }

    /// Reset all counters (e.g., at start of new trading day)
    pub fn reset_counters(&self) {
        self.fatal_error_count.store(0, Ordering::Relaxed);
        self.orders_submitted.store(0, Ordering::Relaxed);
        self.orders_rejected.store(0, Ordering::Relaxed);
        self.half_open_successes.store(0, Ordering::Relaxed);
        *self.window_start.write().unwrap() = Instant::now();
    }

    /// Get current stats
    pub fn stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            state: self.state(),
            fatal_error_count: self.fatal_error_count.load(Ordering::Relaxed),
            orders_submitted: self.orders_submitted.load(Ordering::Relaxed),
            orders_rejected: self.orders_rejected.load(Ordering::Relaxed),
            reject_rate: self.current_reject_rate(),
            open_reason: self.open_reason(),
        }
    }

    // --- Private methods ---

    fn open(&self, reason: OpenReason) {
        let mut state = self.state.write().unwrap();
        if *state != CircuitState::Open {
            error!("Circuit breaker OPEN: {}", reason);
            *state = CircuitState::Open;
            *self.open_reason.write().unwrap() = Some(reason);
            *self.opened_at.write().unwrap() = Some(Instant::now());
            self.half_open_successes.store(0, Ordering::Relaxed);
        }
    }

    fn close(&self) {
        let mut state = self.state.write().unwrap();
        info!("Circuit breaker CLOSED: Trading resumed");
        *state = CircuitState::Closed;
        *self.open_reason.write().unwrap() = None;
        *self.opened_at.write().unwrap() = None;
        self.reset_counters();
    }

    fn transition_to_half_open(&self) {
        let mut state = self.state.write().unwrap();
        info!("Circuit breaker HALF-OPEN: Testing recovery");
        *state = CircuitState::HalfOpen;
        self.half_open_successes.store(0, Ordering::Relaxed);
    }

    fn maybe_transition_to_half_open(&self) {
        let state = *self.state.read().unwrap();
        if state != CircuitState::Open {
            return;
        }

        // Check if enough time has passed
        if let Some(opened_at) = *self.opened_at.read().unwrap() {
            if opened_at.elapsed() >= self.reset_timeout {
                self.transition_to_half_open();
            }
        }
    }

    fn check_reject_rate(&self) {
        let rate = self.current_reject_rate();
        if rate > self.reject_rate_threshold {
            self.open(OpenReason::RejectRateExceeded { rate });
        }
    }

    fn current_reject_rate(&self) -> f64 {
        let submitted = self.orders_submitted.load(Ordering::Relaxed);
        let rejected = self.orders_rejected.load(Ordering::Relaxed);

        if submitted == 0 {
            0.0
        } else {
            rejected as f64 / submitted as f64
        }
    }

    fn maybe_reset_window(&self) {
        let window_start = *self.window_start.read().unwrap();
        if window_start.elapsed() >= self.window_duration {
            // Reset window
            *self.window_start.write().unwrap() = Instant::now();
            self.fatal_error_count.store(0, Ordering::Relaxed);
            self.orders_submitted.store(0, Ordering::Relaxed);
            self.orders_rejected.store(0, Ordering::Relaxed);
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for circuit breaker state
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub fatal_error_count: u32,
    pub orders_submitted: u32,
    pub orders_rejected: u32,
    pub reject_rate: f64,
    pub open_reason: Option<OpenReason>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_trading_allowed());
    }

    #[test]
    fn test_opens_on_fatal_threshold() {
        // Use 100% reject rate threshold so we only test fatal error count
        let cb = CircuitBreaker::with_thresholds(3, 1.0, Duration::from_secs(60));

        cb.record_order_submitted();
        cb.record_order_result(Some(ErrorType::Fatal));
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_order_submitted();
        cb.record_order_result(Some(ErrorType::Fatal));
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_order_submitted();
        cb.record_order_result(Some(ErrorType::Fatal));
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_trading_allowed());
    }

    #[test]
    fn test_ignores_retryable_errors() {
        let cb = CircuitBreaker::with_thresholds(2, 0.5, Duration::from_secs(60));

        // Retryable errors don't count
        for _ in 0..10 {
            cb.record_order_submitted();
            cb.record_order_result(Some(ErrorType::Retryable));
        }
        assert_eq!(cb.state(), CircuitState::Closed);

        // Expected errors don't count
        for _ in 0..10 {
            cb.record_order_submitted();
            cb.record_order_result(Some(ErrorType::Expected));
        }
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_opens_on_ws_disconnect() {
        let cb = CircuitBreaker::new();
        cb.record_ws_disconnect();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(matches!(
            cb.open_reason(),
            Some(OpenReason::WebSocketDisconnect)
        ));
    }

    #[test]
    fn test_half_open_on_ws_reconnect() {
        let cb = CircuitBreaker::new();
        cb.record_ws_disconnect();
        assert_eq!(cb.state(), CircuitState::Open);

        cb.record_ws_reconnect();
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        assert!(cb.is_trading_allowed());
    }

    #[test]
    fn test_closes_after_successful_half_open() {
        let cb = CircuitBreaker::new();
        cb.record_ws_disconnect();
        cb.record_ws_reconnect();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Need 3 successful orders to close
        cb.record_order_result(None);
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_order_result(None);
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_order_result(None);
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_manual_trigger() {
        let cb = CircuitBreaker::new();
        cb.manual_open();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(matches!(cb.open_reason(), Some(OpenReason::ManualTrigger)));

        cb.force_close();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_reject_rate_threshold() {
        let cb = CircuitBreaker::with_thresholds(100, 0.3, Duration::from_secs(60));

        // Submit 10 orders, 4 rejected = 40% reject rate > 30% threshold
        for i in 0..10 {
            cb.record_order_submitted();
            if i < 4 {
                cb.record_order_result(Some(ErrorType::Fatal));
            } else {
                cb.record_order_result(None);
            }
        }

        assert_eq!(cb.state(), CircuitState::Open);
        assert!(matches!(
            cb.open_reason(),
            Some(OpenReason::RejectRateExceeded { .. })
        ));
    }

    #[test]
    fn test_stats() {
        let cb = CircuitBreaker::new();
        cb.record_order_submitted();
        cb.record_order_submitted();
        cb.record_order_result(None);
        cb.record_order_result(Some(ErrorType::Fatal));

        let stats = cb.stats();
        assert_eq!(stats.orders_submitted, 2);
        assert_eq!(stats.orders_rejected, 1);
        assert_eq!(stats.fatal_error_count, 1);
    }
}
