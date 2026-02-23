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

use crate::api::types::{OrderType, Side};
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

        // === PRE-SELL BALANCE SYNCHRONIZATION ===
        // For SELL orders, we must ensure the CLOB's off-chain balance cache
        // reflects our actual on-chain token holdings. Without this:
        //   1. The CLOB cache may still show zero (pre-buy state)
        //   2. The actual token amount may differ from size_matched (fractional slippage)
        //
        // Steps:
        //   1. Force CLOB to refresh cache via on-chain RPC read (AWAIT — not fire-and-forget)
        //   2. Query the refreshed cache for exact fractional balance
        //   3. Use min(requested_size, actual_balance) as sell size
        //
        // This is safe because the 7-second settlement cooldown in momentum.rs
        // ensures tokens are on-chain by the time we reach here.
        let sell_size_override = if intent.side == Side::Sell {
            match self.sync_balance_for_sell(&params.token_id).await {
                Ok(Some(actual_balance)) => {
                    if actual_balance < params.size {
                        warn!(
                            "SELL size adjusted: requested={} actual_balance={} (fractional slippage)",
                            params.size, actual_balance
                        );
                    }
                    if actual_balance <= Decimal::ZERO {
                        error!(
                            "SELL aborted: CLOB reports zero conditional token balance after cache refresh"
                        );
                        return ExecutionResult {
                            intent_token_id: intent.token_id.clone(),
                            order_id: None,
                            filled: false,
                            filled_size: Decimal::ZERO,
                            requested_size: intent.size,
                            status: ExecutionStatus::Rejected,
                            error: Some("Zero conditional token balance after cache refresh".to_string()),
                        };
                    }
                    // Use the smaller of requested and actual (handles fractional slippage)
                    Some(actual_balance.min(params.size))
                }
                Ok(None) => {
                    // Exchange doesn't support balance query — proceed with original size
                    debug!("Balance query not supported, using original sell size");
                    None
                }
                Err(e) => {
                    // Non-fatal: if balance sync fails, still try the sell with original size
                    // (the 7s cooldown should have been enough for the cache to update naturally)
                    warn!("Pre-sell balance sync failed (proceeding anyway): {}", e);
                    None
                }
            }
        } else {
            None
        };

        let final_size = sell_size_override.unwrap_or(params.size);

        // Use cached tick size (warm from startup). No async/network call.
        let tick_size = self.exchange.get_minimum_tick_size_cached(&params.token_id);

        // Convert policy OrderParams to Exchange params
        let exchange_params = ExchangeOrderParams {
            token_id: params.token_id.clone(),
            side: params.side,
            price: params.price,
            size: final_size,
            order_type: self.map_order_type(params.order_type),
            fee_rate_bps: params.fee_rate_bps,
            minimum_tick_size: tick_size,
            // post_only=false: Polymarket CLOB does not currently enforce post-only.
            // Exit sells (emergency, stop-loss) use GTC crossed aggressively at best_bid,
            // which must NOT be post_only or the CLOB would reject them.
            post_only: false,
        };

        // Submit order via Exchange (SDK handles signing and amounts!)
        match self.exchange.place_order(exchange_params).await {
            Ok(order) => {
                let filled = order.filled_size > Decimal::ZERO;
                let status = if order.filled_size >= final_size {
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

    /// Synchronize the CLOB's balance cache before a SELL order.
    ///
    /// This is the critical fix for the "not enough balance / allowance" error:
    ///   1. Forces the CLOB to read our ACTUAL on-chain token balance (RPC call)
    ///   2. Queries the now-refreshed cache for the exact fractional balance
    ///   3. Returns the balance so the caller can use it as the sell size
    ///
    /// This MUST be called only after settlement is complete (≥7s post-buy fill).
    /// The method is synchronous (awaited) to guarantee the cache is updated
    /// before the sell order is submitted.
    async fn sync_balance_for_sell(&self, token_id: &str) -> Result<Option<Decimal>, ExchangeError> {
        // Step 1: Force CLOB to refresh its cached view from on-chain state.
        // This triggers an RPC read to Polygon (100-500ms).
        info!("Pre-sell: refreshing CLOB balance cache (on-chain RPC read)...");
        self.exchange.refresh_balance_cache(token_id).await?;

        // Step 2: Query the now-refreshed cache for exact balance.
        // This handles fractional slippage (Issue #245): actual tokens may
        // differ from size_matched due to fee rounding in CTFExchange.sol.
        let balance = self.exchange.get_conditional_balance(token_id).await?;
        if let Some(bal) = balance {
            info!("Pre-sell: CLOB reports conditional token balance = {}", bal);
        }
        Ok(balance)
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

    /// Execute a grouped set of intents via batch API submission
    ///
    /// Uses the batch POST /orders endpoint to submit all legs in a single HTTP call.
    /// Both orders are built+signed concurrently, then submitted atomically.
    /// If one leg fails after submission, attempts to unwind the filled leg.
    #[cfg(feature = "arb")]
    pub async fn execute_grouped(&self, intents: &[OrderIntent]) -> Vec<ExecutionResult> {
        // For non-grouped or single orders, use individual execution
        if intents.len() != 2 {
            return self.execute_batch(intents).await;
        }

        // Check circuit breaker
        if !self.circuit_breaker.is_trading_allowed() {
            warn!("Circuit breaker open, rejecting grouped order");
            return intents.iter().map(|i| ExecutionResult {
                intent_token_id: i.token_id.clone(),
                order_id: None,
                filled: false,
                filled_size: Decimal::ZERO,
                requested_size: i.size,
                status: ExecutionStatus::CircuitOpen,
                error: Some("Circuit breaker open".to_string()),
            }).collect();
        }

        // Build ExchangeOrderParams for all legs
        let params_list: Vec<_> = intents.iter().map(|intent| {
            let intent_ref = IntentRef::from_intent(intent);
            let params = self.policy.to_order_params(&intent_ref);

            // Use cached tick size (warm from startup), fallback to 0.01
            let tick_size = self.exchange.get_minimum_tick_size_cached(&params.token_id);

            ExchangeOrderParams {
                token_id: params.token_id.clone(),
                side: params.side,
                price: params.price,
                size: params.size,
                order_type: self.map_order_type(params.order_type),
                fee_rate_bps: params.fee_rate_bps,
                minimum_tick_size: tick_size,
                post_only: false,
            }
        }).collect();

        info!(
            legs = params_list.len(),
            "Batch submitting grouped order via POST /orders"
        );

        // Submit all legs via single batch HTTP call
        let batch_results = match self.exchange.place_orders_batch(params_list).await {
            Ok(results) => results,
            Err(e) => {
                // Entire batch failed (network error, etc.)
                error!(error = %e, "Batch submission failed entirely");
                self.circuit_breaker.record_order_result(Some(ErrorType::from(&e)));
                return intents.iter().map(|i| ExecutionResult {
                    intent_token_id: i.token_id.clone(),
                    order_id: None,
                    filled: false,
                    filled_size: Decimal::ZERO,
                    requested_size: i.size,
                    status: ExecutionStatus::SubmissionFailed,
                    error: Some(e.to_string()),
                }).collect();
            }
        };

        // Convert batch results to ExecutionResults
        let results: Vec<ExecutionResult> = batch_results
            .into_iter()
            .zip(intents.iter())
            .map(|(result, intent)| match result {
                Ok(order) => {
                    let filled = order.filled_size > Decimal::ZERO;
                    let status = if order.filled_size >= intent.size {
                        ExecutionStatus::FullyFilled
                    } else if filled {
                        ExecutionStatus::PartialFill
                    } else if matches!(intent.urgency, crate::strategy::traits::Urgency::Immediate) {
                        ExecutionStatus::Cancelled
                    } else {
                        ExecutionStatus::Pending
                    };
                    self.circuit_breaker.record_order_result(None);
                    ExecutionResult {
                        intent_token_id: intent.token_id.clone(),
                        order_id: Some(order.order_id),
                        filled,
                        filled_size: order.filled_size,
                        requested_size: intent.size,
                        status,
                        error: None,
                    }
                }
                Err(e) => {
                    let error_type = ErrorType::from(&e);
                    self.circuit_breaker.record_order_result(Some(error_type));
                    let is_fak_killed = matches!(&e, ExchangeError::FakKilled(_));
                    ExecutionResult {
                        intent_token_id: intent.token_id.clone(),
                        order_id: None,
                        filled: false,
                        filled_size: Decimal::ZERO,
                        requested_size: intent.size,
                        status: if is_fak_killed {
                            ExecutionStatus::Cancelled
                        } else if e.is_retryable() {
                            ExecutionStatus::SubmissionFailed
                        } else {
                            ExecutionStatus::Rejected
                        },
                        error: Some(e.to_string()),
                    }
                }
            })
            .collect();

        // Check for partial failure: one leg accepted, other failed.
        // FAK killed = no liquidity, but if the OTHER leg matched, we have
        // one-sided exposure that needs unwinding.
        if results.len() == 2 {
            let leg1_accepted = results[0].order_id.as_ref().map_or(false, |id| !id.is_empty());
            let leg2_accepted = results[1].order_id.as_ref().map_or(false, |id| !id.is_empty());
            let leg1_failed = results[0].status == ExecutionStatus::Rejected
                || results[0].status == ExecutionStatus::SubmissionFailed
                || results[0].status == ExecutionStatus::Cancelled;
            let leg2_failed = results[1].status == ExecutionStatus::Rejected
                || results[1].status == ExecutionStatus::SubmissionFailed
                || results[1].status == ExecutionStatus::Cancelled;

            // Unwind whichever leg was accepted if the other failed
            let unwind_idx = if leg1_accepted && leg2_failed {
                Some(0)
            } else if leg2_accepted && leg1_failed {
                Some(1)
            } else {
                None
            };

            if let Some(idx) = unwind_idx {
                let other = 1 - idx;
                warn!(
                    accepted_token = %intents[idx].token_id,
                    accepted_order = %results[idx].order_id.as_deref().unwrap_or("?"),
                    accepted_size = %intents[idx].size,
                    failed_token = %intents[other].token_id,
                    failed_error = %results[other].error.as_deref().unwrap_or("?"),
                    "Partial batch failure - cancelling+unwinding accepted leg"
                );

                // Cancel the accepted order to prevent further fills.
                // For FAK orders that already matched (status=Matched), the cancel is a
                // no-op since the order is already complete. But if it's still live,
                // this prevents additional fills that deepen the exposure.
                let order_id = results[idx].order_id.as_deref().unwrap_or("");
                if !order_id.is_empty() {
                    match self.cancel_order(order_id).await {
                        Ok(_) => info!(order_id = %order_id, "Cancelled accepted leg before it could fill more"),
                        Err(e) => info!(order_id = %order_id, error = %e, "Cancel attempt (order may already be matched)"),
                    }
                }

                // NOTE: We do NOT attempt to sell immediately. For FAK orders, the fill
                // has already happened on-chain but shares may not be in our wallet yet
                // (settlement is async). Attempting to sell now fails with "not enough
                // balance / allowance". The one-sided position will be visible in the
                // ledger and can be unwound manually or by a future redemption cycle.
                error!(
                    token = %intents[idx].token_id,
                    order_id = %order_id,
                    size = %intents[idx].size,
                    price = %intents[idx].price,
                    "ONE-SIDED EXPOSURE: Accepted leg filled but other leg failed. Manual intervention may be needed."
                );
            }
        }

        results
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
