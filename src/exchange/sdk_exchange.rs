//! SDK-based Exchange implementation
//!
//! This module implements the Exchange trait using the official
//! polymarket-client-sdk. It handles:
//! - Correct amount calculation (SDK uses trunc_with_scale)
//! - EIP-712 order signing
//! - L2 HMAC authentication
//! - Error mapping

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

// SDK imports - matching the official example pattern
use alloy::signers::local::LocalSigner;
use alloy::signers::Signer as AlloySigner;
use polymarket_client_sdk::clob::types::request::{BalanceAllowanceRequest, OrdersRequest};
use polymarket_client_sdk::clob::types::{AssetType, OrderStatusType, OrderType as SdkOrderType, Side as SdkSide};
use polymarket_client_sdk::clob::{Client, Config as ClobConfig};
use polymarket_client_sdk::POLYGON;

use crate::api::types::Side;
use crate::exchange::errors::ExchangeError;
use crate::exchange::traits::{Exchange, ExchangeOrderParams, ExchangeOrderType, ExchangeResult};
use crate::exchange::types::{DomainOrder, OrderId, OrderStatus};

// Type alias for the signer type - same as SDK uses internally
type PrivateKeySigner = LocalSigner<k256::ecdsa::SigningKey>;

/// SDK-based exchange implementation
///
/// This implementation wraps the polymarket-client-sdk and handles:
/// - Amount calculation using SDK's correct truncation
/// - Order signing via SDK's EIP-712 implementation
/// - Response mapping to domain types
pub struct SdkExchange {
    /// Authenticated SDK client
    client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,

    /// Signer for order signing (same one used for authentication)
    signer: PrivateKeySigner,

    /// Maker address (proxy wallet)
    maker_address: String,

    /// Signer address (EOA)
    signer_addr: String,

    /// Whether this is a neg-risk market
    #[allow(dead_code)]
    is_neg_risk: bool,

    /// Cached tick sizes per token
    tick_sizes: DashMap<String, Decimal>,

    /// Health status
    healthy: AtomicBool,
}

/// Parse a token ID string (decimal integer) into U256.
/// Polymarket token IDs are large decimal integers like "48331043336612883890938759509493603899..."
fn parse_token_u256(s: &str) -> U256 {
    U256::from_str(s).unwrap_or_else(|_| {
        // Fallback: try parsing as decimal
        U256::from_str_radix(s, 10).unwrap_or_else(|e| {
            panic!("Invalid token ID '{}': {}", s, e);
        })
    })
}

impl SdkExchange {
    /// Create a new SDK exchange
    ///
    /// # Arguments
    /// * `private_key` - Hex-encoded private key for signing (with or without 0x prefix)
    /// * `maker_address` - Proxy wallet address (funder)
    /// * `is_neg_risk` - Whether to use neg-risk exchange
    pub async fn new(
        private_key: String,
        maker_address: String,
        is_neg_risk: bool,
    ) -> ExchangeResult<Self> {
        // Parse private key and create signer (same pattern as SDK example)
        let signer = LocalSigner::from_str(&private_key)
            .map_err(|e| ExchangeError::Configuration(format!("Invalid private key: {}", e)))?
            .with_chain_id(Some(POLYGON));

        let signer_addr = signer.address().to_checksum(None);

        info!(
            signer = %signer_addr,
            maker = %maker_address,
            neg_risk = %is_neg_risk,
            "Initializing SDK exchange"
        );

        // Use local time — avoids GET /time HTTP call on every authenticated request.
        // NTP-synced VPS keeps local clock within milliseconds of server time,
        // well within the L2 HMAC validation window.
        let config = ClobConfig::builder()
            .use_server_time(false)
            .build();

        // Create and authenticate client (SDK handles credential creation)
        let client = Client::new("https://clob.polymarket.com", config)
            .map_err(|e| ExchangeError::Configuration(format!("Failed to create client: {}", e)))?
            .authentication_builder(&signer)
            .authenticate()
            .await
            .map_err(|e| {
                ExchangeError::AuthenticationFailed(format!("SDK authentication failed: {}", e))
            })?;

        // Verify connection with a health check
        match client.ok().await {
            Ok(_) => info!("SDK exchange initialized and authenticated successfully"),
            Err(e) => {
                warn!(error = %e, "Health check failed during initialization");
                // Don't fail initialization - might be transient
            }
        }

        Ok(Self {
            client,
            signer,
            maker_address,
            signer_addr,
            is_neg_risk,
            tick_sizes: DashMap::new(),
            healthy: AtomicBool::new(true),
        })
    }

    /// Map our Side to SDK Side
    fn map_side(side: Side) -> SdkSide {
        match side {
            Side::Buy => SdkSide::Buy,
            Side::Sell => SdkSide::Sell,
        }
    }

    /// Map SDK Side to our Side
    fn map_sdk_side(side: SdkSide) -> Side {
        match side {
            SdkSide::Buy => Side::Buy,
            SdkSide::Sell => Side::Sell,
            SdkSide::Unknown | _ => Side::Buy, // Default to Buy for unknown variants
        }
    }

    /// Map our OrderType to SDK OrderType
    fn map_order_type(order_type: ExchangeOrderType) -> SdkOrderType {
        match order_type {
            ExchangeOrderType::GTC => SdkOrderType::GTC,
            ExchangeOrderType::FOK => SdkOrderType::FOK,
            ExchangeOrderType::FAK => SdkOrderType::FAK,
            ExchangeOrderType::GTD { .. } => SdkOrderType::GTD,
        }
    }

    /// Parse order status from SDK OrderStatusType
    fn map_order_status(status: &OrderStatusType) -> OrderStatus {
        match status {
            OrderStatusType::Live => OrderStatus::Open,
            OrderStatusType::Matched => OrderStatus::Filled,
            OrderStatusType::Canceled => OrderStatus::Cancelled,
            OrderStatusType::Delayed => OrderStatus::Pending,
            OrderStatusType::Unmatched => OrderStatus::Pending,
            OrderStatusType::Unknown(_) | _ => OrderStatus::Pending,
        }
    }

}

#[async_trait]
impl Exchange for SdkExchange {
    async fn warm_caches(&self, token_ids: &[String]) {
        let start = Instant::now();
        info!(count = token_ids.len(), "Pre-warming SDK caches (tick_size, fee_rate, neg_risk)...");

        // Process in batches of 10 to avoid overwhelming the API
        for chunk in token_ids.chunks(10) {
            let futs: Vec<_> = chunk.iter().map(|token_id| async {
                let token_u256 = parse_token_u256(token_id);

                // These populate the SDK's internal DashMap caches
                let ts = self.client.tick_size(token_u256).await;
                let _ = self.client.fee_rate_bps(token_u256).await;
                let _ = self.client.neg_risk(token_u256).await;

                // Also populate our own tick_sizes cache
                if let Ok(resp) = ts {
                    let tick: Decimal = resp.minimum_tick_size.into();
                    self.tick_sizes.insert(token_id.clone(), tick);
                }
            }).collect();
            futures_util::future::join_all(futs).await;
        }

        info!(elapsed_ms = start.elapsed().as_millis() as u64, count = token_ids.len(), "SDK caches warmed");
    }

    async fn place_orders_batch(
        &self,
        params_list: Vec<ExchangeOrderParams>,
    ) -> ExchangeResult<Vec<ExchangeResult<DomainOrder>>> {
        if params_list.is_empty() {
            return Ok(vec![]);
        }
        if params_list.len() == 1 {
            return Ok(vec![self.place_order(params_list.into_iter().next().unwrap()).await]);
        }

        let batch_start = Instant::now();
        info!(count = params_list.len(), "Batch building+signing orders...");

        // Step 1: Build all orders concurrently (SDK caches should be warm)
        let build_futs: Vec<_> = params_list.iter().map(|p| {
            let token_u256 = parse_token_u256(&p.token_id);
            let mut builder = self
                .client
                .limit_order()
                .token_id(token_u256)
                .side(Self::map_side(p.side))
                .price(p.price)
                .size(p.size)
                .order_type(Self::map_order_type(p.order_type));

            if let ExchangeOrderType::GTD { expiration } = p.order_type {
                let expiration_dt = chrono::DateTime::from_timestamp(expiration as i64, 0)
                    .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(24));
                builder = builder.expiration(expiration_dt);
            }

            async move { builder.build().await }
        }).collect();

        let build_results = futures_util::future::join_all(build_futs).await;
        let build_ms = batch_start.elapsed().as_millis();

        // Collect signable orders, tracking which succeeded
        let mut signable_orders = Vec::with_capacity(params_list.len());
        let mut build_errors: Vec<Option<ExchangeError>> = Vec::with_capacity(params_list.len());

        for result in build_results {
            match result {
                Ok(signable) => {
                    signable_orders.push(Some(signable));
                    build_errors.push(None);
                }
                Err(e) => {
                    error!(error = %e, "Batch order build failed");
                    signable_orders.push(None);
                    build_errors.push(Some(ExchangeError::InvalidParams(
                        format!("Order build failed: {}", e),
                    )));
                }
            }
        }

        // If ALL builds failed, return errors
        if signable_orders.iter().all(|o| o.is_none()) {
            return Ok(build_errors.into_iter().zip(params_list.iter()).map(|(err, _p)| {
                Err(err.unwrap_or_else(|| ExchangeError::InvalidParams("Build failed".into())))
            }).collect());
        }

        // Step 2: Sign all successfully built orders concurrently
        let sign_start = Instant::now();
        let sign_futs: Vec<_> = signable_orders.iter_mut().filter_map(|o| {
            o.take().map(|signable| {
                self.client.sign(&self.signer, signable)
            })
        }).collect();

        let sign_results = futures_util::future::join_all(sign_futs).await;
        let sign_ms = sign_start.elapsed().as_millis();

        // Reassemble: merge sign results back with build errors
        let mut signed_orders = Vec::new();
        let mut sign_iter = sign_results.into_iter();
        let mut final_errors: Vec<Option<ExchangeError>> = Vec::with_capacity(params_list.len());

        for build_err in &build_errors {
            if build_err.is_some() {
                final_errors.push(build_err.clone());
            } else {
                match sign_iter.next() {
                    Some(Ok(signed)) => {
                        signed_orders.push(signed);
                        final_errors.push(None);
                    }
                    Some(Err(e)) => {
                        error!(error = %e, "Batch order sign failed");
                        final_errors.push(Some(ExchangeError::Signing(
                            format!("Signing failed: {}", e),
                        )));
                    }
                    None => {
                        final_errors.push(Some(ExchangeError::Signing("No sign result".into())));
                    }
                }
            }
        }

        // If no orders were signed successfully, return all errors
        if signed_orders.is_empty() {
            return Ok(final_errors.into_iter().zip(params_list.iter()).map(|(err, _p)| {
                Err(err.unwrap_or_else(|| ExchangeError::Signing("Sign failed".into())))
            }).collect());
        }

        // Step 3: Submit ALL signed orders in single HTTP call
        let submit_start = Instant::now();
        info!(count = signed_orders.len(), "Submitting batch via POST /orders");
        let responses = self.client.post_orders(signed_orders).await.map_err(|e| {
            error!(error = %e, "Batch order submission failed");
            self.healthy.store(false, Ordering::Relaxed);
            ExchangeError::Network(e.to_string())
        })?;

        let submit_ms = submit_start.elapsed().as_millis();
        let total_ms = batch_start.elapsed().as_millis();
        self.healthy.store(true, Ordering::Relaxed);

        info!(
            build_ms = build_ms,
            sign_ms = sign_ms,
            submit_ms = submit_ms,
            total_ms = total_ms,
            "[PERF] Batch order: build/sign/submit"
        );

        // Step 4: Map responses back to results, interleaving with errors
        let mut resp_iter = responses.into_iter();
        let mut results = Vec::with_capacity(params_list.len());

        for (i, final_err) in final_errors.into_iter().enumerate() {
            if let Some(err) = final_err {
                results.push(Err(err));
            } else if let Some(response) = resp_iter.next() {
                // Log FULL response for every leg so we can diagnose empty order_id issues
                info!(
                    leg = i,
                    token = %params_list[i].token_id,
                    success = response.success,
                    order_id = %response.order_id,
                    status = ?response.status,
                    making_amount = %response.making_amount,
                    taking_amount = %response.taking_amount,
                    error_msg = ?response.error_msg,
                    tx_hashes = ?response.transaction_hashes,
                    trade_ids = ?response.trade_ids,
                    "Batch leg response"
                );

                if !response.success || response.order_id.is_empty() {
                    let error_msg = response.error_msg.as_deref().unwrap_or("");
                    let error_lower = error_msg.to_lowercase();

                    // Classify the error based on the server's error message
                    let exchange_err = if error_lower.contains("no orders found to match with fak")
                        || error_lower.contains("no orders found to match with fok")
                    {
                        // FAK/FOK killed — normal, no liquidity. Not a real error.
                        info!(
                            token = %params_list[i].token_id,
                            "FAK order killed (no matching liquidity)"
                        );
                        ExchangeError::FakKilled(error_msg.to_string())
                    } else if error_lower.contains("invalid amounts") {
                        warn!(
                            token = %params_list[i].token_id,
                            error = %error_msg,
                            "Batch leg rejected: invalid amounts"
                        );
                        ExchangeError::InvalidParams(error_msg.to_string())
                    } else {
                        let msg = if response.order_id.is_empty() && response.success {
                            format!("Empty order_id: {}", error_msg)
                        } else {
                            error_msg.to_string()
                        };
                        warn!(
                            token = %params_list[i].token_id,
                            empty_id = response.order_id.is_empty(),
                            success = response.success,
                            "Batch leg rejected: {}",
                            msg
                        );
                        ExchangeError::OrderRejected {
                            code: "REJECTED".to_string(),
                            message: msg,
                        }
                    };
                    results.push(Err(exchange_err));
                } else {
                    let p = &params_list[i];
                    info!(
                        order_id = %response.order_id,
                        token = %p.token_id,
                        side = ?p.side,
                        price = %p.price,
                        size = %p.size,
                        "Batch order placed successfully"
                    );
                    results.push(Ok(DomainOrder {
                        order_id: response.order_id,
                        token_id: p.token_id.clone(),
                        side: p.side,
                        price: p.price,
                        original_size: p.size,
                        remaining_size: p.size,
                        filled_size: Decimal::ZERO,
                        status: Self::map_order_status(&response.status),
                        created_at: Utc::now(),
                        maker_amount: response.making_amount,
                        taker_amount: response.taking_amount,
                    }));
                }
            } else {
                results.push(Err(ExchangeError::Network("Missing response for order".into())));
            }
        }

        Ok(results)
    }

    async fn place_order(&self, params: ExchangeOrderParams) -> ExchangeResult<DomainOrder> {
        debug!(
            token = %params.token_id,
            side = ?params.side,
            price = %params.price,
            size = %params.size,
            order_type = ?params.order_type,
            tick_size = %params.minimum_tick_size,
            "Building order via SDK"
        );

        let order_start = Instant::now();

        // Build order using SDK - SDK handles amount calculation correctly!
        let token_u256 = parse_token_u256(&params.token_id);
        let mut builder = self
            .client
            .limit_order()
            .token_id(token_u256)
            .side(Self::map_side(params.side))
            .price(params.price)
            .size(params.size)
            .order_type(Self::map_order_type(params.order_type));

        // Add expiration for GTD orders
        if let ExchangeOrderType::GTD { expiration } = params.order_type {
            // Convert timestamp to DateTime
            let expiration_dt = chrono::DateTime::from_timestamp(expiration as i64, 0)
                .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(24));
            builder = builder.expiration(expiration_dt);
        }

        // Build the signable order
        let signable_order = builder.build().await.map_err(|e| {
            error!(error = %e, "SDK order build failed");
            ExchangeError::InvalidParams(format!("Order build failed: {}", e))
        })?;

        let build_ms = order_start.elapsed().as_millis();

        // Sign the order using SDK (same signer used for auth)
        let sign_start = Instant::now();
        let signed_order = self
            .client
            .sign(&self.signer, signable_order)
            .await
            .map_err(|e| {
                error!(error = %e, "SDK signing failed");
                ExchangeError::Signing(format!("Signing failed: {}", e))
            })?;

        let sign_ms = sign_start.elapsed().as_millis();

        // Submit the order
        let submit_start = Instant::now();
        let response = self.client.post_order(signed_order).await.map_err(|e| {
            error!(error = %e, "Order submission failed");
            self.healthy.store(false, Ordering::Relaxed);

            // Try to extract status code and message
            let err_str = e.to_string();
            if err_str.contains("400") {
                ExchangeError::from_api_error(400, &err_str)
            } else if err_str.contains("401") {
                ExchangeError::AuthenticationFailed(err_str)
            } else if err_str.contains("429") {
                ExchangeError::RateLimited(err_str)
            } else {
                ExchangeError::Network(err_str)
            }
        })?;

        let submit_ms = submit_start.elapsed().as_millis();
        let total_ms = order_start.elapsed().as_millis();

        // Check for error in response
        if !response.success {
            let msg = response.error_msg.unwrap_or_else(|| "Unknown error".to_string());
            info!(
                build_ms = build_ms,
                sign_ms = sign_ms,
                submit_ms = submit_ms,
                total_ms = total_ms,
                "[PERF] Order REJECTED: build/sign/submit"
            );
            return Err(ExchangeError::OrderRejected {
                code: "REJECTED".to_string(),
                message: msg,
            });
        }

        // Mark as healthy on success
        self.healthy.store(true, Ordering::Relaxed);

        info!(
            build_ms = build_ms,
            sign_ms = sign_ms,
            submit_ms = submit_ms,
            total_ms = total_ms,
            order_id = %response.order_id,
            "[PERF] Order placed: build/sign/submit"
        );

        // Create domain order from response
        Ok(DomainOrder {
            order_id: response.order_id.clone(),
            token_id: params.token_id.clone(),
            side: params.side,
            price: params.price,
            original_size: params.size,
            remaining_size: params.size, // Will be updated by fills
            filled_size: Decimal::ZERO,
            status: Self::map_order_status(&response.status),
            created_at: Utc::now(),
            // SDK calculated these correctly - we don't need to track them
            maker_amount: response.making_amount,
            taker_amount: response.taking_amount,
        })
    }

    async fn cancel_order(&self, order_id: &OrderId) -> ExchangeResult<()> {
        debug!(order_id = %order_id, "Cancelling order via SDK");

        self.client.cancel_order(order_id).await.map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("not found") || err_str.contains("404") {
                ExchangeError::OrderNotFound(order_id.clone())
            } else {
                ExchangeError::Network(err_str)
            }
        })?;

        info!(order_id = %order_id, "Order cancelled");
        Ok(())
    }

    async fn get_order(&self, order_id: &OrderId) -> ExchangeResult<OrderStatus> {
        let request = OrdersRequest::builder().order_id(order_id).build();
        let response = self.client.orders(&request, None).await.map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("not found") || err_str.contains("404") {
                ExchangeError::OrderNotFound(order_id.clone())
            } else {
                ExchangeError::Network(err_str)
            }
        })?;

        // Find the order in the response
        response
            .data
            .into_iter()
            .find(|o| o.id == *order_id)
            .map(|o| Self::map_order_status(&o.status))
            .ok_or_else(|| ExchangeError::OrderNotFound(order_id.clone()))
    }

    async fn get_open_orders(&self) -> ExchangeResult<Vec<DomainOrder>> {
        let request = OrdersRequest::default();
        let response = self
            .client
            .orders(&request, None)
            .await
            .map_err(|e| ExchangeError::Network(format!("Failed to get orders: {}", e)))?;

        let domain_orders = response
            .data
            .into_iter()
            .filter(|o| o.status == OrderStatusType::Live)
            .map(|o| DomainOrder {
                order_id: o.id,
                token_id: o.asset_id.to_string(),
                side: Self::map_sdk_side(o.side),
                price: o.price,
                original_size: o.original_size,
                remaining_size: o.original_size - o.size_matched,
                filled_size: o.size_matched,
                status: Self::map_order_status(&o.status),
                created_at: o.created_at,
                maker_amount: Decimal::ZERO,
                taker_amount: Decimal::ZERO,
            })
            .collect();

        Ok(domain_orders)
    }

    async fn cancel_orders_for_token(&self, token_id: &str) -> ExchangeResult<Vec<OrderId>> {
        let open_orders = self.get_open_orders().await?;

        let token_orders: Vec<_> = open_orders
            .iter()
            .filter(|o| o.token_id == token_id && o.is_active())
            .map(|o| o.order_id.clone())
            .collect();

        let mut cancelled = Vec::new();
        for order_id in token_orders {
            match self.cancel_order(&order_id).await {
                Ok(_) => cancelled.push(order_id),
                Err(ExchangeError::OrderNotFound(_)) => {
                    // Already cancelled/filled, ignore
                }
                Err(e) => {
                    warn!(order_id = %order_id, error = %e, "Failed to cancel order");
                }
            }
        }

        Ok(cancelled)
    }

    async fn cancel_all_orders(&self) -> ExchangeResult<Vec<OrderId>> {
        debug!("Cancelling all orders via SDK");

        let response = self
            .client
            .cancel_all_orders()
            .await
            .map_err(|e| ExchangeError::Network(format!("Failed to cancel all orders: {}", e)))?;

        let cancelled: Vec<OrderId> = response
            .canceled
            .into_iter()
            .map(|id| id.to_string())
            .collect();

        info!(count = cancelled.len(), "Cancelled all orders");
        Ok(cancelled)
    }

    async fn get_minimum_tick_size(&self, token_id: &str) -> ExchangeResult<Decimal> {
        // Check cache first
        if let Some(tick) = self.tick_sizes.get(token_id) {
            return Ok(*tick);
        }

        // Query from API
        let token_u256 = parse_token_u256(token_id);
        let response = self
            .client
            .tick_size(token_u256)
            .await
            .map_err(|e| ExchangeError::Network(format!("Failed to get tick size: {}", e)))?;

        // Convert TickSize enum to Decimal
        let tick_size: Decimal = response.minimum_tick_size.into();

        // Cache it
        self.tick_sizes.insert(token_id.to_string(), tick_size);

        debug!(token = %token_id, tick_size = %tick_size, "Cached tick size");
        Ok(tick_size)
    }

    fn get_minimum_tick_size_cached(&self, token_id: &str) -> Decimal {
        self.tick_sizes
            .get(token_id)
            .map(|v| *v)
            .unwrap_or(Decimal::new(1, 2))
    }

    async fn get_balance(&self) -> ExchangeResult<Decimal> {
        let request = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();

        let response = self
            .client
            .balance_allowance(request)
            .await
            .map_err(|e| ExchangeError::Network(format!("Failed to get balance: {}", e)))?;

        // API returns raw micro-USDC integer (e.g. 50351130 = $50.35 USDC)
        // USDC has 6 decimals, so divide by 10^6
        let usdc_divisor = Decimal::from(1_000_000);
        let balance = response.balance / usdc_divisor;

        debug!(raw = %response.balance, balance_usd = %balance, "Fetched USDC balance from exchange");
        Ok(balance)
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    fn maker_address(&self) -> &str {
        &self.maker_address
    }

    fn signer_address(&self) -> &str {
        &self.signer_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_side_mapping() {
        assert!(matches!(SdkExchange::map_side(Side::Buy), SdkSide::Buy));
        assert!(matches!(SdkExchange::map_side(Side::Sell), SdkSide::Sell));
    }

    #[test]
    fn test_order_type_mapping() {
        assert!(matches!(
            SdkExchange::map_order_type(ExchangeOrderType::GTC),
            SdkOrderType::GTC
        ));
        assert!(matches!(
            SdkExchange::map_order_type(ExchangeOrderType::FOK),
            SdkOrderType::FOK
        ));
    }

    #[test]
    fn test_status_mapping() {
        assert_eq!(
            SdkExchange::map_order_status(&OrderStatusType::Live),
            OrderStatus::Open
        );
        assert_eq!(
            SdkExchange::map_order_status(&OrderStatusType::Matched),
            OrderStatus::Filled
        );
        assert_eq!(
            SdkExchange::map_order_status(&OrderStatusType::Canceled),
            OrderStatus::Cancelled
        );
    }
}
