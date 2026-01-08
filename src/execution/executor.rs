//! Order executor - converts intents to orders and submits them
//!
//! The executor is the bridge between strategy decisions (OrderIntent)
//! and actual order submission. It:
//! 1. Applies ExecutionPolicy to convert intent → OrderParams
//! 2. Signs orders using the OrderSigner
//! 3. Submits orders to the exchange
//! 4. Handles partial fills per policy rules
//! 5. Tracks execution results

use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::api::types::{OrderRequest, OrderResponse, OrderType};
use crate::api::ApiClient;
use crate::error::ErrorType;
use crate::execution::policy::{ExecutionPolicy, IntentRef, OrderParams};
use crate::risk::circuit_breaker::CircuitBreaker;
use crate::signing::{Order, OrderBuilder, OrderSigner};
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

/// Executes order intents by converting them to orders and submitting
pub struct OrderExecutor {
    /// API client for order submission
    client: Arc<ApiClient>,

    /// Order signer for EIP-712 signatures
    signer: Arc<OrderSigner>,

    /// Execution policy (determines order type, partial fill handling)
    policy: Arc<dyn ExecutionPolicy>,

    /// Circuit breaker to check before submission
    circuit_breaker: Arc<CircuitBreaker>,

    /// Maker address (proxy/funder wallet where funds are held)
    maker_address: String,

    /// Whether this is a neg-risk market (affects signing)
    is_neg_risk: bool,
}

impl OrderExecutor {
    /// Create a new order executor
    ///
    /// # Arguments
    /// * `maker_address` - Proxy wallet address (funder, where funds are held)
    pub fn new(
        client: Arc<ApiClient>,
        signer: Arc<OrderSigner>,
        policy: Arc<dyn ExecutionPolicy>,
        circuit_breaker: Arc<CircuitBreaker>,
        maker_address: String,
        is_neg_risk: bool,
    ) -> Self {
        // Ensure maker address is checksummed (EIP-55 format)
        let checksummed_maker = crate::signing::to_checksum_address(&maker_address)
            .unwrap_or_else(|_| maker_address.clone());

        Self {
            client,
            signer,
            policy,
            circuit_breaker,
            maker_address: checksummed_maker,
            is_neg_risk,
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
            "Executing order"
        );

        // Build and sign the order
        let order = self.build_order(&params);
        let signed_order = match self.sign_order(&order).await {
            Ok(signed) => signed,
            Err(e) => {
                error!(error = %e, "Failed to sign order");
                return ExecutionResult {
                    intent_token_id: intent.token_id.clone(),
                    order_id: None,
                    filled: false,
                    filled_size: Decimal::ZERO,
                    requested_size: intent.size,
                    status: ExecutionStatus::SubmissionFailed,
                    error: Some(format!("Signing failed: {}", e)),
                };
            }
        };

        // Create order request
        // IMPORTANT: owner = API key string (NOT an address!)
        // This is how Polymarket's API validates the request
        let api_key = self.client.credentials().api_key.clone();
        let request = OrderRequest {
            defer_exec: false,
            order: signed_order,
            owner: api_key, // API key, not address!
            order_type: params.order_type,
        };

        // Submit order
        match self.client.place_order(&request).await {
            Ok(response) => self.process_response(&params, response),
            Err(e) => {
                error!(error = %e, "Order submission failed");
                // Record failure for circuit breaker (treat network errors as retryable)
                self.circuit_breaker
                    .record_order_result(Some(ErrorType::Retryable));
                ExecutionResult {
                    intent_token_id: intent.token_id.clone(),
                    order_id: None,
                    filled: false,
                    filled_size: Decimal::ZERO,
                    requested_size: intent.size,
                    status: ExecutionStatus::SubmissionFailed,
                    error: Some(e.to_string()),
                }
            }
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
                // (Could use futures_util::future::join_all for parallel if needed)
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
            || result1.status == ExecutionStatus::CircuitOpen {
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
            && result1.status == ExecutionStatus::Pending {

            if let Some(ref order_id) = result1.order_id {
                warn!(
                    order_id = %order_id,
                    "Second leg failed, cancelling first leg to prevent one-legged exposure"
                );
                match self.client.cancel_order(order_id).await {
                    Ok(_) => {
                        info!(order_id = %order_id, "Successfully cancelled first leg");
                    }
                    Err(e) => {
                        // Cancel failed - this is critical!
                        // The first leg may have filled or is being matched
                        error!(
                            order_id = %order_id,
                            error = %e,
                            "🚨 CRITICAL: Failed to cancel first leg - ORPHANED POSITION!"
                        );
                        error!(
                            token_id = %intents[0].token_id,
                            market_id = %intents[0].market_id,
                            side = ?intents[0].side,
                            price = %intents[0].price,
                            size = %intents[0].size,
                            "🚨 Orphaned position details - MANUAL REVIEW REQUIRED"
                        );
                        // Trip the circuit breaker to prevent further damage
                        self.circuit_breaker.record_order_result(Some(ErrorType::Critical));
                    }
                }
            }
        }

        vec![result1, result2]
    }

    /// Build an order from params
    fn build_order(&self, params: &OrderParams) -> Order {
        let signer_address = self.signer.address();

        OrderBuilder::new(
            self.maker_address.clone(),
            signer_address,
            params.token_id.clone(),
            params.side,
        )
        .with_price_size(params.price, params.size)
        .with_expiration(params.expiration)
        .with_fee_rate_bps(params.fee_rate_bps)
        .build()
    }

    /// Sign an order (handles neg-risk vs standard)
    async fn sign_order(
        &self,
        order: &Order,
    ) -> Result<crate::api::types::SignedOrder, crate::error::BotError> {
        if self.is_neg_risk {
            self.signer.sign_order_neg_risk(order).await
        } else {
            self.signer.sign_order(order).await
        }
    }

    /// Process order response into execution result
    fn process_response(&self, params: &OrderParams, response: OrderResponse) -> ExecutionResult {
        if response.success {
            // Parse filled amount if available
            let filled_size = response
                .taking_amount
                .parse::<Decimal>()
                .unwrap_or(Decimal::ZERO);

            let filled = filled_size > Decimal::ZERO;

            let status = if filled_size >= params.size {
                ExecutionStatus::FullyFilled
            } else if filled {
                ExecutionStatus::PartialFill
            } else if params.order_type == OrderType::FOK {
                ExecutionStatus::Cancelled
            } else {
                ExecutionStatus::Pending
            };

            info!(
                order_id = %response.order_id,
                status = ?status,
                filled = %filled_size,
                requested = %params.size,
                "Order executed"
            );

            // Record success for circuit breaker
            self.circuit_breaker.record_order_result(None);

            ExecutionResult {
                intent_token_id: params.token_id.clone(),
                order_id: Some(response.order_id),
                filled,
                filled_size,
                requested_size: params.size,
                status,
                error: None,
            }
        } else {
            warn!(
                error = %response.error_msg,
                token = %params.token_id,
                "Order rejected"
            );

            // Classify error and record for circuit breaker
            let error_type = ErrorType::from_error_msg(&response.error_msg);
            self.circuit_breaker.record_order_result(Some(error_type));

            ExecutionResult {
                intent_token_id: params.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: params.size,
                status: ExecutionStatus::Rejected,
                error: Some(response.error_msg),
            }
        }
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
