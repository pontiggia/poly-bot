//! Order executor - converts intents to orders and submits them
//!
//! The executor is the bridge between strategy decisions (OrderIntent)
//! and actual order submission. It:
//! 1. Applies ExecutionPolicy to convert intent → OrderParams
//! 2. Uses the Exchange trait to submit orders (SDK handles signing/amounts)
//! 3. Handles partial fills per policy rules
//! 4. Tracks execution results

use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::api::types::OrderType;
use crate::error::ErrorType;
use crate::exchange::{Exchange, ExchangeError, ExchangeOrderParams, ExchangeOrderType};
use crate::execution::policy::{ExecutionPolicy, IntentRef};
use crate::risk::circuit_breaker::CircuitBreaker;
use crate::strategy::OrderIntent;
use rust_decimal::Decimal;

// ============================================================================
// EXECUTION RESULT
// ============================================================================

/// Result of executing an order intent
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Original intent
    pub intent_token_id: String,

    /// Order ID if submission succeeded
    pub order_id: Option<String>,

    /// Whether the order was filled (any amount)
    pub filled: bool,

    /// Amount filled (if any)
    pub filled_size: Decimal,

    /// Original requested size
    pub requested_size: Decimal,

    /// Execution status
    pub status: ExecutionStatus,

    /// Error message if failed
    pub error: Option<String>,
}

/// Status of order execution
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Order fully filled
    FullyFilled,

    /// Order partially filled
    PartialFill,

    /// Order accepted, pending fill
    Pending,

    /// Order rejected by exchange
    Rejected,

    /// Order cancelled (FOK that didn't fill)
    Cancelled,

    /// Failed to submit
    SubmissionFailed,

    /// Circuit breaker prevented submission
    CircuitOpen,
}

// ============================================================================
// ORDER EXECUTOR
// ============================================================================

/// Executes order intents by converting them to orders and submitting via Exchange
pub struct OrderExecutor {
    /// Exchange for order submission (SDK-based)
    exchange: Arc<dyn Exchange>,

    /// Execution policy (determines order type, partial fill handling)
    policy: Arc<dyn ExecutionPolicy>,

    /// Circuit breaker to check before submission
    circuit_breaker: Arc<CircuitBreaker>,
}

impl OrderExecutor {
    /// Create a new order executor
    ///
    /// # Arguments
    /// * `exchange` - Exchange implementation (SDK handles signing/amounts)
    /// * `policy` - Execution policy for order type selection
    /// * `circuit_breaker` - Circuit breaker for risk management
    pub fn new(
        exchange: Arc<dyn Exchange>,
        policy: Arc<dyn ExecutionPolicy>,
        circuit_breaker: Arc<CircuitBreaker>,
    ) -> Self {
        Self {
            exchange,
            policy,
            circuit_breaker,
        }
    }

    /// Execute a single order intent
    pub async fn execute(&self, intent: &OrderIntent) -> ExecutionResult {
        // Check circuit breaker
        if !self.circuit_breaker.is_trading_allowed() {
            warn!(
                strategy = %intent.strategy_name,
                token = %intent.token_id,
                "Circuit breaker open, rejecting order"
            );
            return ExecutionResult {
                intent_token_id: intent.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: intent.size,
                status: ExecutionStatus::CircuitOpen,
                error: Some("Circuit breaker open".to_string()),
            };
        }

        // Convert intent to order params using policy
        let intent_ref = IntentRef::from_intent(intent);
        let params = self.policy.to_order_params(&intent_ref);

        debug!(
            token = %params.token_id,
            side = ?params.side,
            price = %params.price,
            size = %params.size,
            order_type = ?params.order_type,
            policy = %self.policy.name(),
            "Executing order via SDK"
        );

        // Get tick size for proper amount calculation
        let tick_size = match self.exchange.get_minimum_tick_size(&params.token_id).await {
            Ok(ts) => ts,
            Err(e) => {
                error!(error = %e, "Failed to get tick size");
                // Default to 0.01 if we can't fetch tick size
                Decimal::new(1, 2)
            }
        };

        // Convert policy OrderParams to Exchange params
        let exchange_params = ExchangeOrderParams {
            token_id: params.token_id.clone(),
            side: params.side,
            price: params.price,
            size: params.size,
            order_type: self.map_order_type(params.order_type),
            fee_rate_bps: params.fee_rate_bps,
            minimum_tick_size: tick_size,
            post_only: params.order_type == OrderType::GTC, // GTC orders can be post-only
        };

        // Submit order via Exchange (SDK handles signing and amounts!)
        match self.exchange.place_order(exchange_params).await {
            Ok(order) => {
                let filled = order.filled_size > Decimal::ZERO;
                let status = if order.filled_size >= params.size {
                    ExecutionStatus::FullyFilled
                } else if filled {
                    ExecutionStatus::PartialFill
                } else if params.order_type == OrderType::FOK {
                    ExecutionStatus::Cancelled
                } else {
                    ExecutionStatus::Pending
                };

                info!(
                    order_id = %order.order_id,
                    status = ?status,
                    filled = %order.filled_size,
                    requested = %params.size,
                    "Order executed via SDK"
                );

                // Record success for circuit breaker
                self.circuit_breaker.record_order_result(None);

                ExecutionResult {
                    intent_token_id: params.token_id.clone(),
                    order_id: Some(order.order_id),
                    filled,
                    filled_size: order.filled_size,
                    requested_size: params.size,
                    status,
                    error: None,
                }
            }
            Err(e) => {
                error!(error = %e, "Order submission failed");

                // Classify error for circuit breaker
                let error_type = ErrorType::from(&e);
                self.circuit_breaker.record_order_result(Some(error_type));

                ExecutionResult {
                    intent_token_id: intent.token_id.clone(),
                    order_id: None,
                    filled: false,
                    filled_size: Decimal::ZERO,
                    requested_size: intent.size,
                    status: if e.is_retryable() {
                        ExecutionStatus::SubmissionFailed
                    } else {
                        ExecutionStatus::Rejected
                    },
                    error: Some(e.to_string()),
                }
            }
        }
    }

    /// Map policy OrderType to Exchange OrderType
    fn map_order_type(&self, order_type: OrderType) -> ExchangeOrderType {
        match order_type {
            OrderType::GTC => ExchangeOrderType::GTC,
            OrderType::FOK => ExchangeOrderType::FOK,
            OrderType::FAK => ExchangeOrderType::FAK,
        }
    }

    /// Execute multiple intents concurrently (for multi-leg orders like arb)
    ///
    /// This uses tokio::join! to submit all orders at once, minimizing
    /// the time window between leg submissions.
    pub async fn execute_batch(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        match intents.len() {
            0 => vec![],
            1 => vec![self.execute(&intents[0]).await],
            2 => {
                // Common case: two-leg arb
                let (r1, r2) = tokio::join!(self.execute(&intents[0]), self.execute(&intents[1]));
                vec![r1, r2]
            }
            3 => {
                let (r1, r2, r3) = tokio::join!(
                    self.execute(&intents[0]),
                    self.execute(&intents[1]),
                    self.execute(&intents[2])
                );
                vec![r1, r2, r3]
            }
            _ => {
                // For larger batches, execute sequentially
                let mut results = Vec::with_capacity(intents.len());
                for intent in intents {
                    results.push(self.execute(intent).await);
                }
                results
            }
        }
    }

    /// Execute a grouped set of intents with sequential submission and rollback
    ///
    /// For grouped orders (like arb legs), we execute sequentially to ensure
    /// we can cancel the first leg if the second fails, preventing one-legged exposure.
    pub async fn execute_grouped(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        // For non-grouped or single orders, use batch execution
        if intents.len() != 2 {
            return self.execute_batch(intents).await;
        }

        // Execute first leg
        let result1 = self.execute(&intents[0]).await;

        // If first leg failed completely (submission error), don't execute second
        if result1.status == ExecutionStatus::SubmissionFailed
            || result1.status == ExecutionStatus::CircuitOpen
        {
            warn!(
                token = %intents[0].token_id,
                error = ?result1.error,
                "First leg failed, skipping second leg to prevent exposure"
            );
            let result2 = ExecutionResult {
                intent_token_id: intents[1].token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: intents[1].size,
                status: ExecutionStatus::Cancelled,
                error: Some("Skipped: first leg failed".to_string()),
            };
            return vec![result1, result2];
        }

        // Execute second leg
        let result2 = self.execute(&intents[1]).await;

        // If second leg failed but first succeeded with pending order, cancel first
        if (result2.status == ExecutionStatus::SubmissionFailed
            || result2.status == ExecutionStatus::Rejected)
            && result1.order_id.is_some()
            && result1.status == ExecutionStatus::Pending
        {
            if let Some(ref order_id) = result1.order_id {
                warn!(
                    order_id = %order_id,
                    "Second leg failed, cancelling first leg to prevent one-legged exposure"
                );
                match self.exchange.cancel_order(order_id).await {
                    Ok(_) => {
                        info!(order_id = %order_id, "Successfully cancelled first leg");
                    }
                    Err(e) => {
                        // Cancel failed - this is critical!
                        error!(
                            order_id = %order_id,
                            error = %e,
                            "CRITICAL: Failed to cancel first leg - ORPHANED POSITION!"
                        );
                        error!(
                            token_id = %intents[0].token_id,
                            market_id = %intents[0].market_id,
                            side = ?intents[0].side,
                            price = %intents[0].price,
                            size = %intents[0].size,
                            "Orphaned position details - MANUAL REVIEW REQUIRED"
                        );
                        // Trip the circuit breaker to prevent further damage
                        self.circuit_breaker
                            .record_order_result(Some(ErrorType::Critical));
                    }
                }
            }
        }

        vec![result1, result2]
    }

    /// Cancel an order by ID
    pub async fn cancel_order(&self, order_id: &str) -> Result<(), ExchangeError> {
        self.exchange.cancel_order(&order_id.to_string()).await
    }

    /// Check if exchange is healthy
    pub fn is_exchange_healthy(&self) -> bool {
        self.exchange.is_healthy()
    }

    /// Get maker address
    pub fn maker_address(&self) -> &str {
        self.exchange.maker_address()
    }

    /// Get signer address
    pub fn signer_address(&self) -> &str {
        self.exchange.signer_address()
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_status_eq() {
        assert_eq!(ExecutionStatus::FullyFilled, ExecutionStatus::FullyFilled);
        assert_ne!(ExecutionStatus::FullyFilled, ExecutionStatus::PartialFill);
    }

    #[test]
    fn test_execution_result_creation() {
        let result = ExecutionResult {
            intent_token_id: "token123".to_string(),
            order_id: Some("order456".to_string()),
            filled: true,
            filled_size: Decimal::from(100),
            requested_size: Decimal::from(100),
            status: ExecutionStatus::FullyFilled,
            error: None,
        };

        assert!(result.filled);
        assert_eq!(result.filled_size, result.requested_size);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_partial_fill_detection() {
        let result = ExecutionResult {
            intent_token_id: "token".to_string(),
            order_id: Some("order".to_string()),
            filled: true,
            filled_size: Decimal::from(50),
            requested_size: Decimal::from(100),
            status: ExecutionStatus::PartialFill,
            error: None,
        };

        assert!(result.filled_size < result.requested_size);
        assert_eq!(result.status, ExecutionStatus::PartialFill);
    }
}
