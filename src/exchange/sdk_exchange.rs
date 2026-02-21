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

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use rust_decimal::Decimal;
use tracing::{debug, error, info, warn};

// SDK imports - matching the official example pattern
use alloy::signers::local::LocalSigner;
use alloy::signers::Signer as AlloySigner;
use polymarket_client_sdk::clob::types::request::OrdersRequest;
use polymarket_client_sdk::clob::types::{OrderStatusType, OrderType as SdkOrderType, Side as SdkSide};
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

        // Create config - use server time for better sync
        let config = ClobConfig::builder()
            .use_server_time(true)
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
            // SDK marks Side as non-exhaustive
            _ => Side::Buy, // Default to Buy for unknown variants
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
            // SDK marks OrderStatusType as non-exhaustive
            _ => OrderStatus::Pending,
        }
    }

}

#[async_trait]
impl Exchange for SdkExchange {
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

        // Build order using SDK - SDK handles amount calculation correctly!
        let mut builder = self
            .client
            .limit_order()
            .token_id(&params.token_id)
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

        debug!(
            order_type = ?signable_order.order_type,
            "Order built successfully, signing..."
        );

        // Sign the order using SDK (same signer used for auth)
        let signed_order = self
            .client
            .sign(&self.signer, signable_order)
            .await
            .map_err(|e| {
                error!(error = %e, "SDK signing failed");
                ExchangeError::Signing(format!("Signing failed: {}", e))
            })?;

        debug!("Order signed, submitting...");

        // Submit the order
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

        // Check for error in response
        if !response.success {
            let msg = response.error_msg.unwrap_or_else(|| "Unknown error".to_string());
            return Err(ExchangeError::OrderRejected {
                code: "REJECTED".to_string(),
                message: msg,
            });
        }

        // Mark as healthy on success
        self.healthy.store(true, Ordering::Relaxed);

        info!(
            order_id = %response.order_id,
            token = %params.token_id,
            side = ?params.side,
            price = %params.price,
            size = %params.size,
            "Order placed successfully via SDK"
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
                token_id: o.asset_id,
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
        let response = self
            .client
            .tick_size(token_id)
            .await
            .map_err(|e| ExchangeError::Network(format!("Failed to get tick size: {}", e)))?;

        // Convert TickSize enum to Decimal
        let tick_size: Decimal = response.minimum_tick_size.into();

        // Cache it
        self.tick_sizes.insert(token_id.to_string(), tick_size);

        debug!(token = %token_id, tick_size = %tick_size, "Cached tick size");
        Ok(tick_size)
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
