//! Bot orchestration - connects all components together
//!
//! This module handles the main event loop and coordinates between:
//! - WebSocket market data (order book updates)
//! - WebSocket user data (fill notifications)
//! - Strategy execution
//! - Risk management
//!
//! ## Event-Driven Architecture
//!
//! The bot uses `tokio::select!` for zero-latency event handling:
//! - Market WS messages processed instantly (<1ms)
//! - User WS fills processed instantly
//! - Periodic tick for strategy logic (100ms)
//! - Heartbeat for logging (10s)
//! - Async kill signal for shutdown

use crate::config::{Config, OperatingMode};
use crate::exchange::{Exchange, ExchangeError, SdkExchange};
use crate::execution::{DualPolicy, ExecutionResult, ExecutionStatus, OrderExecutor, OrderTracker, TrackedOrder};
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::risk::CircuitBreaker;
use crate::state::{OrderBookState, PriceHistory, SpotPriceState};
use crate::strategy::{
    MarketPair, MarketPairRegistry, OrderAction, OrderIntent,
    StrategyContext, StrategyRouter,
};
use crate::websocket::{BinanceWebSocket, MarketMessage, MarketWebSocket, UserMessage, UserWebSocket};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

/// Main bot struct that orchestrates all components
pub struct Bot {
    /// Configuration
    config: Arc<Config>,
    /// Kill switch for emergency stop
    kill_switch: Arc<KillSwitch>,
    /// Order book state (shared across components)
    order_book_state: Arc<OrderBookState>,
    /// Ledger for tracking orders, positions, cash
    ledger: Arc<Ledger>,
    /// Market pair registry (YES/NO token mappings)
    market_registry: Arc<MarketPairRegistry>,
    /// Strategy router
    strategy_router: Arc<StrategyRouter>,
    /// Circuit breaker for risk management
    circuit_breaker: Arc<CircuitBreaker>,
    /// Exchange for direct operations (balance, cancel all)
    exchange: Arc<dyn Exchange>,
    /// Order executor for submitting trades
    executor: Arc<OrderExecutor>,
    /// Order tracker for outstanding GTC orders
    order_tracker: Arc<OrderTracker>,
    /// Market WebSocket message receiver
    market_ws_rx: mpsc::UnboundedReceiver<MarketMessage>,
    /// Market WebSocket task handle
    market_ws_task: JoinHandle<()>,
    /// User WebSocket message receiver
    user_ws_rx: mpsc::UnboundedReceiver<UserMessage>,
    /// User WebSocket task handle
    user_ws_task: JoinHandle<()>,
    /// Last log time per token (for rate limiting)
    last_log_time: HashMap<String, Instant>,
    /// Message counter per token
    message_counts: HashMap<String, u64>,
    /// Total messages processed
    total_messages: u64,
    /// Total order intents generated
    total_intents: u64,
    /// Total orders executed
    total_executions: u64,
    /// Total fills received
    total_fills: u64,
    /// Seen trade IDs (for deduplication)
    seen_trade_ids: HashSet<String>,
    /// Subscription sender for dynamic WS market subscriptions
    ws_subscription_tx: mpsc::UnboundedSender<Vec<String>>,
    /// Spot price state from Binance (BTC/ETH/SOL/XRP)
    spot_prices: Arc<SpotPriceState>,
    /// Price history for momentum calculation
    price_history: Arc<PriceHistory>,
}

impl Bot {
    /// Create a new bot instance
    ///
    /// # Arguments
    /// * `config` - Bot configuration
    /// * `kill_switch` - Kill switch for emergency stop
    /// * `token_ids` - Token IDs to subscribe to (alternating YES/NO pairs)
    /// * `market_pairs` - Market pair definitions for arb detection
    pub async fn new(
        config: Config,
        kill_switch: Arc<KillSwitch>,
        token_ids: Vec<String>,
        market_pairs: Vec<MarketPair>,
    ) -> Self {
        let config = Arc::new(config);
        let order_book_state = Arc::new(OrderBookState::new());

        // Set up market pair registry
        let market_registry = Arc::new(MarketPairRegistry::new());
        for pair in market_pairs {
            market_registry.register(pair);
        }

        // Set up strategy router
        let strategy_router = Arc::new(StrategyRouter::new());

        // Register MomentumSniper strategy (5m + 15m markets)
        let momentum_config = crate::strategy::MomentumConfig::default_live_test();
        info!(
            "Registering MomentumSniper: conviction>={}, maker_buy={}, TP={}, max_exposure=${}",
            momentum_config.min_conviction,
            momentum_config.maker_buy_price,
            momentum_config.take_profit_price,
            momentum_config.max_total_exposure,
        );
        let momentum = Arc::new(crate::strategy::MomentumStrategy::new(
            market_registry.clone(),
            momentum_config,
        ));
        if let Err(e) = strategy_router.register(momentum.clone()) {
            warn!("Failed to register MomentumStrategy: {}", e);
        }

        // Set up circuit breaker for risk management
        let circuit_breaker = Arc::new(CircuitBreaker::new());

        // Create SDK exchange - handles authentication and signing internally
        // The SDK derives credentials from private key using L1 auth flow
        let exchange = SdkExchange::new(
            config.private_key.clone(),
            config.wallet_address.clone(), // maker = proxy wallet (funder)
            false, // TODO: Detect neg-risk from market data
        )
        .await
        .expect("Failed to create SDK exchange");

        // Pre-warm SDK caches: tick_size, fee_rate, neg_risk for all tokens.
        // Eliminates 3 HTTP calls per first-time order build+sign.
        exchange.warm_caches(&token_ids).await;

        let exchange: Arc<dyn Exchange> = Arc::new(exchange);

        // Log addresses
        info!("EOA Signer address: {}", exchange.signer_address());
        info!("Proxy wallet (funder): {}", exchange.maker_address());

        // Fetch actual USDC balance from exchange for accurate ledger initialization
        let initial_balance = match exchange.get_balance().await {
            Ok(balance) => {
                info!("Fetched USDC balance from exchange: ${}", balance);
                balance
            }
            Err(e) => {
                warn!("Failed to fetch balance from exchange: {}. Using config max_bet as fallback.", e);
                config.max_bet_usd
            }
        };
        // Re-create ledger with actual balance
        let ledger = Arc::new(Ledger::new(initial_balance));

        // Use DualPolicy: Taker for Immediate/Normal, Maker for Passive
        // Maker orders post inside spread for better fill probability
        let policy = Arc::new(
            DualPolicy::new()
                .with_maker_offset(config.maker_price_offset)
        );

        info!(
            "Execution policy: DualPolicy (Taker=FOK/FAK, Maker=GTC offset={} cents)",
            config.maker_price_offset
        );

        // Create executor with SDK exchange (handles signing/amounts correctly)
        let executor = Arc::new(OrderExecutor::new(
            exchange.clone(),
            policy,
            circuit_breaker.clone(),
        ));

        // Set up order tracker for outstanding orders
        let order_tracker = Arc::new(OrderTracker::new());

        // Set up Market WebSocket for order book data (with dynamic subscription support)
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let (market_ws, ws_subscription_tx) = MarketWebSocket::new(token_ids.clone(), market_ws_tx);

        // Spawn Market WebSocket task
        let market_ws_clone = market_ws.clone();
        let market_ws_task = tokio::spawn(async move {
            market_ws_clone.run().await;
        });

        // Set up User WebSocket for fill notifications
        // Use user credentials if available, otherwise fall back to builder credentials
        let (user_ws_tx, user_ws_rx) = mpsc::unbounded_channel();
        let (user_api_key, user_secret, user_pass) = if config.has_user_credentials() {
            info!("Using USER credentials for User WebSocket (fills)");
            (
                config.user_api_key.clone().unwrap(),
                config.user_secret_key.clone().unwrap(),
                config.user_passphrase.clone().unwrap(),
            )
        } else {
            warn!("No USER credentials configured - using builder credentials for User WebSocket");
            warn!("  Hint: Set USER_API_KEY, USER_SECRET_KEY, USER_PASSPHRASE for fill notifications");
            (
                config.api_key.clone(),
                config.secret_key.clone(),
                config.passphrase.clone(),
            )
        };

        let user_ws = Arc::new(UserWebSocket::new(
            user_api_key,
            user_secret,
            user_pass,
            user_ws_tx,
        ));

        // Spawn User WebSocket task
        let user_ws_clone = user_ws.clone();
        let user_ws_task = tokio::spawn(async move {
            user_ws_clone.run().await;
        });

        // Set up Binance WebSocket for spot prices
        let spot_prices = Arc::new(SpotPriceState::new());
        let price_history = Arc::new(PriceHistory::new(1800)); // 30 min at ~1/sec (supports 15m lookback)
        let binance_ws = Arc::new(BinanceWebSocket::new(
            spot_prices.clone(),
            price_history.clone(),
        ));
        let binance_ws_clone = binance_ws.clone();
        tokio::spawn(async move {
            binance_ws_clone.run().await;
        });

        info!(
            "Bot initialized: {} token(s), {} market pair(s), {} strateg(ies)",
            token_ids.len(),
            market_registry.len(),
            strategy_router.strategy_names().len()
        );

        Self {
            config,
            kill_switch,
            order_book_state,
            ledger,
            market_registry,
            strategy_router,
            circuit_breaker,
            exchange,
            executor,
            order_tracker,
            market_ws_rx,
            market_ws_task,
            user_ws_rx,
            user_ws_task,
            last_log_time: HashMap::new(),
            message_counts: HashMap::new(),
            total_messages: 0,
            total_intents: 0,
            total_executions: 0,
            total_fills: 0,
            seen_trade_ids: HashSet::new(),
            ws_subscription_tx,
            spot_prices,
            price_history,
        }
    }

    /// Run the main event loop (event-driven architecture)
    ///
    /// Uses `tokio::select!` for zero-latency event handling:
    /// - Market WS: Processed instantly when received
    /// - User WS: Fill notifications processed instantly
    /// - Tick: Every 100ms for strategy periodic logic
    /// - Heartbeat: Every 10s for logging/monitoring
    /// - Kill signal: Async shutdown trigger
    pub async fn run(&mut self) {
        info!("Starting bot main loop (event-driven)...");

        // Periodic tick for strategy logic (100ms)
        let mut tick_interval = interval(Duration::from_millis(100));

        // Heartbeat for logging (10s)
        let mut heartbeat_interval = interval(Duration::from_secs(10));

        // Stale order cleanup (30s)
        let mut stale_cleanup_interval = interval(Duration::from_secs(30));

        // Balance reconciliation (60s)
        let mut balance_reconcile_interval = interval(Duration::from_secs(60));

        // Order management tick (100ms) — cancel/replace loop for maker orders
        let mut order_mgmt_interval = interval(Duration::from_millis(100));

        loop {
            tokio::select! {
                // Bias toward market data - process first if multiple ready
                biased;

                // Market WebSocket messages - highest priority, zero latency
                Some(msg) = self.market_ws_rx.recv() => {
                    self.handle_market_message(msg).await;
                }

                // User WebSocket messages - fill notifications
                Some(msg) = self.user_ws_rx.recv() => {
                    self.handle_user_message(msg).await;
                }

                // Strategy tick - 100ms periodic
                _ = tick_interval.tick() => {
                    self.handle_tick().await;
                }

                // Order management tick — cancel/replace loop for maker orders
                _ = order_mgmt_interval.tick() => {
                    self.handle_order_management().await;
                }

                // Stale order cleanup - 30s periodic
                _ = stale_cleanup_interval.tick() => {
                    self.cleanup_stale_orders().await;
                }

                // Balance reconciliation - 60s periodic
                _ = balance_reconcile_interval.tick() => {
                    self.reconcile_balance().await;
                }

                // Heartbeat - 10s periodic logging
                _ = heartbeat_interval.tick() => {
                    self.log_heartbeat();
                }

                // Kill signal - graceful shutdown
                _ = self.kill_switch.wait_for_kill() => {
                    warn!("Kill signal received - shutting down");
                    break;
                }
            }
        }

        // Cleanup
        self.shutdown().await;
    }

    /// Handle a market WebSocket message
    async fn handle_market_message(&mut self, msg: MarketMessage) {
        self.total_messages += 1;

        match msg {
            MarketMessage::Connected => {
                info!("WebSocket connected to market data stream");
            }
            MarketMessage::Reconnecting => {
                warn!("WebSocket reconnecting...");
            }
            MarketMessage::BookSnapshot(book_msg) => {
                self.handle_book_snapshot(book_msg).await;
            }
            MarketMessage::LevelUpdate(level_msg) => {
                self.handle_level_update(level_msg).await;
            }
        }
    }

    /// Handle a user WebSocket message (fills, order updates)
    async fn handle_user_message(&mut self, msg: UserMessage) {
        match msg {
            UserMessage::Connected => {
                info!("User WebSocket connected - receiving fill notifications");
            }
            UserMessage::Reconnecting => {
                warn!("User WebSocket reconnecting...");
            }
            UserMessage::Trade(trade) => {
                self.handle_trade_notification(trade).await;
            }
            UserMessage::OrderUpdate(update) => {
                self.handle_order_update(update).await;
            }
        }
    }

    /// Handle a trade/fill notification
    async fn handle_trade_notification(&mut self, trade: crate::websocket::TradeNotification) {
        // Deduplicate: skip if we've already seen this trade ID
        if !trade.id.is_empty() && self.seen_trade_ids.contains(&trade.id) {
            debug!("Skipping duplicate trade notification: {}", &trade.id[..trade.id.len().min(12)]);
            return;
        }

        // ✅ FIX: Get our order IDs based on whether we're maker or taker
        // When we're MAKER, our order ID is in maker_orders[], not taker_order_id
        let our_order_ids = trade.our_order_ids();
        if our_order_ids.is_empty() {
            debug!("Trade notification has no order_id, skipping (likely market data)");
            return;
        }
        
        // Check if ANY of these order IDs are ones we're tracking
        // CRITICAL: Also verify the token_id matches to prevent cross-token fill misattribution
        let matched_order_id = our_order_ids
            .iter()
            .find(|id| {
                let order_id_str = id.to_string();
                if let Some(tracked) = self.order_tracker.get(&order_id_str) {
                    // Must match BOTH order_id AND token_id
                    if tracked.token_id == trade.asset_id {
                        true
                    } else {
                        // Order ID matches but token doesn't - this is a critical mismatch!
                        warn!(
                            "⚠️ TOKEN MISMATCH: Fill for token {}... claims order {}... which tracks token {}...",
                            &trade.asset_id[..trade.asset_id.len().min(12)],
                            &id[..id.len().min(16)],
                            &tracked.token_id[..tracked.token_id.len().min(12)]
                        );
                        false
                    }
                } else {
                    false
                }
            });
        
        let order_id = match matched_order_id {
            Some(id) => id.to_string(),
            None => {
                // Not our order - could be a market trade we're just seeing
                debug!(
                    "Trade for unknown order(s) {:?}, not ours - skipping (size: {}, price: {})",
                    our_order_ids.iter().map(|id| &id[..id.len().min(16)]).collect::<Vec<_>>(),
                    trade.size,
                    trade.price
                );
                return;
            }
        };

        // Mark as seen
        if !trade.id.is_empty() {
            self.seen_trade_ids.insert(trade.id.clone());
        }

        self.total_fills += 1;

        // Convert to Fill using the matched order ID (correctly uses matched_amount for makers)
        match trade.to_fill_for_order(&order_id) {
            Ok(fill) => {
                // ✅ FIX: Log whether this was maker or taker
                let is_maker = trade.trader_side.to_uppercase() == "MAKER";
                let execution_type = if is_maker {
                    "MAKER ✅"
                } else {
                    "TAKER ⚠️"
                };
                
                info!(
                    "💰 Fill [{}]: {} {} {} @ ${} (fee: ${:.6}, order: {})",
                    execution_type,
                    format!("{:?}", fill.side),
                    fill.size,
                    &fill.token_id[..fill.token_id.len().min(12)],
                    fill.price,
                    fill.fee,
                    &order_id[..order_id.len().min(16)]
                );

                // Record fill in ledger
                self.ledger.process_fill(fill.clone());

                // Notify strategies of fill (may trigger sell orders)
                let fill_ctx = StrategyContext::new(&self.order_book_state, &self.ledger)
                    .with_spot(&self.spot_prices, &self.price_history);
                let fill_intents = self.strategy_router.on_fill(&fill, &fill_ctx);
                if !fill_intents.is_empty() {
                    self.process_intents(fill_intents);
                }

                // Update order tracker
                if let Some(remaining) = self.order_tracker.on_fill(&fill.order_id, fill.size) {
                    if remaining.is_zero() {
                        info!("Order {} fully filled", &fill.order_id[..fill.order_id.len().min(12)]);
                    } else {
                        debug!("Order {} partial fill, {} remaining", &fill.order_id[..fill.order_id.len().min(12)], remaining);
                    }
                }
            }
            Err(e) => {
                error!("Failed to parse trade notification: {}", e);
            }
        }
    }

    /// Handle an order update (ack, cancel, etc.)
    async fn handle_order_update(&mut self, update: crate::websocket::OrderUpdate) {
        debug!(
            "Order update: {} -> {}",
            &update.order_id[..update.order_id.len().min(12)],
            update.status
        );

        // Handle order status changes
        match update.status.to_lowercase().as_str() {
            "cancelled" | "canceled" => {
                self.order_tracker.remove(&update.order_id);
                info!("Order {} cancelled", &update.order_id[..update.order_id.len().min(12)]);
            }
            "expired" => {
                self.order_tracker.remove(&update.order_id);
                info!("Order {} expired", &update.order_id[..update.order_id.len().min(12)]);
            }
            _ => {
                // Other statuses (acked, etc.) - just log
            }
        }
    }

    /// Handle periodic tick (100ms)
    async fn handle_tick(&mut self) {
        // Create strategy context
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger)
            .with_spot(&self.spot_prices, &self.price_history);

        // Run strategy on_tick() callbacks
        let intents = self.strategy_router.on_tick(&ctx);

        // Process any generated intents
        if !intents.is_empty() {
            self.process_intents(intents);
        }
    }

    /// Handle order management tick (100ms) — cancel/replace loop for maker orders
    async fn handle_order_management(&mut self) {
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger)
            .with_spot(&self.spot_prices, &self.price_history);

        let actions = self.strategy_router.on_order_management(&ctx);

        if actions.is_empty() {
            return;
        }

        // Process actions directly through executor (low-latency path)
        let executor = self.executor.clone();
        let circuit_breaker = self.circuit_breaker.clone();
        let order_tracker = self.order_tracker.clone();

        for action in actions {
            match action {
                OrderAction::Cancel { order_id } => {
                    let _ = executor.cancel_order(&order_id).await;
                    order_tracker.remove(&order_id);
                }
                OrderAction::Replace { old_order_id, new_intent } => {
                    // Cancel old
                    let _ = executor.cancel_order(&old_order_id).await;
                    order_tracker.remove(&old_order_id);
                    // Submit new directly
                    let result = executor.execute(&new_intent).await;
                    Self::handle_execution_result(
                        &new_intent, &result, &circuit_breaker, &order_tracker,
                    );
                    self.total_executions += 1;
                }
                OrderAction::PostTakerFallback { intent } => {
                    let result = executor.execute(&intent).await;
                    Self::handle_execution_result(
                        &intent, &result, &circuit_breaker, &order_tracker,
                    );
                    self.total_executions += 1;
                }
            }
        }
    }

    /// Log heartbeat with current stats
    fn log_heartbeat(&mut self) {
        let mode_str = match self.config.mode {
            OperatingMode::Paper => "PAPER",
            OperatingMode::Live => "LIVE",
        };
        let circuit_status = if self.circuit_breaker.is_trading_allowed() {
            "✅"
        } else {
            "🔴 OPEN"
        };
        
        let active_orders = self.order_tracker.active_count();
        
        // Build spot price summary
        let spot_info = if !self.spot_prices.is_empty() {
            let mut parts = Vec::new();
            for asset in &["btc", "eth", "sol", "xrp"] {
                if let Some(p) = self.spot_prices.price(asset) {
                    parts.push(format!("{}=${}", asset.to_uppercase(), p));
                }
            }
            parts.join(" ")
        } else {
            "no spot data".to_string()
        };

        info!(
            "Heartbeat [{}]: {} markets | {} msgs | {:.1} msg/s | {} intents | {} execs | {} fills | {} active | CB: {} | {}",
            mode_str,
            self.order_book_state.num_markets(),
            self.total_messages,
            self.total_messages as f64 / 10.0,  // msgs per second (over 10s window)
            self.total_intents,
            self.total_executions,
            self.total_fills,
            active_orders,
            circuit_status,
            spot_info
        );
        // Reset counter for next interval
        self.total_messages = 0;
    }

    /// Handle a full book snapshot message
    async fn handle_book_snapshot(&mut self, book_msg: crate::websocket::BookUpdateMessage) {
        // Full book replacement
        self.order_book_state.update_book(
            book_msg.token_id.clone(),
            book_msg.market.clone(),
            book_msg.bids,
            book_msg.asks,
            book_msg.timestamp,
            book_msg.hash,
        );

        // Route to strategies
        self.route_book_update(&book_msg.market, &book_msg.token_id);

        self.log_book_state(&book_msg.token_id);
    }

    /// Handle an incremental level update
    async fn handle_level_update(&mut self, level_msg: crate::websocket::LevelUpdateMessage) {
        // Update single price level
        self.order_book_state.update_level(
            &level_msg.token_id,
            level_msg.market.clone(),
            &level_msg.side,
            &level_msg.price,
            &level_msg.size,
            level_msg.timestamp,
            level_msg.hash,
        );

        // Route to strategies
        self.route_book_update(&level_msg.market, &level_msg.token_id);

        self.log_book_state(&level_msg.token_id);
    }

    /// Route a book update to strategies and process intents
    fn route_book_update(&mut self, market_id: &str, token_id: &str) {
        let strategy_start = Instant::now();

        // Create strategy context
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger)
            .with_spot(&self.spot_prices, &self.price_history);

        // Route to strategies
        let intents = self.strategy_router.on_book_update(
            &market_id.to_string(),
            &token_id.to_string(),
            &ctx,
        );

        // Process any generated intents
        if !intents.is_empty() {
            let strategy_us = strategy_start.elapsed().as_micros();
            info!(
                strategy_eval_us = strategy_us,
                intents = intents.len(),
                "[PERF] Strategy evaluation"
            );
            self.process_intents(intents);
        }
    }

    /// Process order intents from strategies
    fn process_intents(&mut self, intents: Vec<OrderIntent>) {
        self.total_intents += intents.len() as u64;

        for intent in &intents {
            let exec_mode = match intent.urgency {
                crate::strategy::Urgency::Immediate => "TAKER/FOK",
                crate::strategy::Urgency::Normal => "TAKER/FAK",
                crate::strategy::Urgency::Passive => "MAKER/GTC",
            };
            info!(
                "📝 Intent: {} {} {} @ ${:.4} x {} [{}] → {}",
                intent.strategy_name,
                format!("{:?}", intent.side),
                &intent.token_id[..intent.token_id.len().min(12)],
                intent.price,
                intent.size,
                intent.reason,
                exec_mode
            );
        }

        // Check circuit breaker first
        if !self.circuit_breaker.is_trading_allowed() {
            warn!("⚠️ Circuit breaker OPEN - not executing {} intent(s)", intents.len());
            return;
        }

        // Check operating mode
        match self.config.mode {
            OperatingMode::Paper => {
                info!(
                    "📋 PAPER MODE: Would execute {} order(s) - not submitting",
                    intents.len()
                );
                // In paper mode, we just log what would happen
                for intent in &intents {
                    info!(
                        "  [PAPER] {} {} @ ${:.4} x {}",
                        format!("{:?}", intent.side),
                        &intent.token_id[..intent.token_id.len().min(16)],
                        intent.price,
                        intent.size
                    );
                }
            }
            OperatingMode::Live => {
                // Spawn execution as background task to not block event loop
                let executor = self.executor.clone();
                let circuit_breaker = self.circuit_breaker.clone();
                let order_tracker = self.order_tracker.clone();
                let intents_owned = intents.clone();
                
                tokio::spawn(async move {
                    Self::execute_intents(executor, circuit_breaker, order_tracker, intents_owned).await;
                });
                
                self.total_executions += intents.len() as u64;
            }
        }
    }

    /// Execute intents asynchronously (called from spawned task)
    async fn execute_intents(
        executor: Arc<OrderExecutor>,
        circuit_breaker: Arc<CircuitBreaker>,
        order_tracker: Arc<OrderTracker>,
        intents: Vec<OrderIntent>,
    ) {
        let exec_start = Instant::now();

        // Calculate total USDC required for all orders
        let total_cost: rust_decimal::Decimal = intents
            .iter()
            .map(|i| i.price * i.size)
            .sum();

        info!(
            "🚀 LIVE: Executing {} order(s) - Total USDC required: ${:.2}",
            intents.len(),
            total_cost
        );

        let results = executor.execute_batch(&intents).await;

        let exec_ms = exec_start.elapsed().as_millis();
        info!(
            exec_total_ms = exec_ms,
            orders = results.len(),
            "[PERF] Order execution complete"
        );

        // Process results
        for (intent, result) in intents.iter().zip(results.iter()) {
            Self::handle_execution_result(intent, result, &circuit_breaker, &order_tracker);
        }
    }

    /// Handle the result of an execution
    fn handle_execution_result(
        intent: &OrderIntent,
        result: &ExecutionResult,
        circuit_breaker: &CircuitBreaker,
        order_tracker: &OrderTracker,
    ) {
        // ✅ FIX: Track pending/partial orders so we can match fills later
        if let Some(ref order_id) = result.order_id {
            if result.status == ExecutionStatus::Pending || result.status == ExecutionStatus::PartialFill {
                let tracked = TrackedOrder {
                    order_id: order_id.clone(),
                    token_id: intent.token_id.clone(),
                    market_id: intent.market_id.clone(),
                    side: intent.side,
                    price: intent.price,
                    original_size: intent.size,
                    filled_size: result.filled_size,
                    created_at: std::time::Instant::now(),
                    strategy_name: intent.strategy_name.clone(),
                    group_id: intent.group_id.clone(),
                };
                order_tracker.track(tracked);
                debug!(
                    "Tracking order {} for fill matching",
                    &order_id[..order_id.len().min(16)]
                );
            }
        }

        match result.status {
            ExecutionStatus::FullyFilled => {
                info!(
                    "✅ FILLED: {} {} @ {} x {} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.filled_size,
                    result.order_id.as_deref().unwrap_or("?")
                );
                circuit_breaker.record_order_result(None);
            }
            ExecutionStatus::PartialFill => {
                warn!(
                    "⚠️ PARTIAL: {} {} @ {} - filled {}/{} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.filled_size,
                    result.requested_size,
                    result.order_id.as_deref().unwrap_or("?")
                );
                circuit_breaker.record_order_result(None);
            }
            ExecutionStatus::Pending => {
                info!(
                    "⏳ PENDING: {} {} @ {} (order: {})",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.order_id.as_deref().unwrap_or("?")
                );
            }
            ExecutionStatus::Rejected => {
                error!(
                    "❌ REJECTED: {} {} @ {} - {}",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.error.as_deref().unwrap_or("unknown error")
                );
                circuit_breaker.record_order_result(Some(crate::error::ErrorType::Expected));
            }
            ExecutionStatus::Cancelled => {
                info!(
                    "🚫 KILLED: {} {} @ {} (no matching liquidity)",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                );
            }
            ExecutionStatus::SubmissionFailed => {
                error!(
                    "💥 FAILED: {} {} @ {} - {}",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                    result.error.as_deref().unwrap_or("submission failed")
                );
                circuit_breaker.record_order_result(Some(crate::error::ErrorType::Retryable));
            }
            ExecutionStatus::CircuitOpen => {
                warn!(
                    "🔴 CIRCUIT OPEN: {} {} @ {} - trading halted",
                    format!("{:?}", intent.side),
                    &intent.token_id[..intent.token_id.len().min(16)],
                    intent.price,
                );
            }
        }
    }

    /// Log current book state (rate limited - max 1 per second per token)
    fn log_book_state(&mut self, token_id: &str) {
        // Increment message count
        *self.message_counts.entry(token_id.to_string()).or_insert(0) += 1;

        // Rate limit: only log once per second per token
        let now = Instant::now();
        let should_log = self
            .last_log_time
            .get(token_id)
            .map(|last| now.duration_since(*last).as_secs() >= 1)
            .unwrap_or(true);

        if !should_log {
            return;
        }

        let token_id_string = token_id.to_string();

        // Log significant updates (for debugging)
        if let (Some(bid), Some(ask)) = (
            self.order_book_state.best_bid(&token_id_string),
            self.order_book_state.best_ask(&token_id_string),
        ) {
            // Only log if spread is reasonable (< 50%)
            let spread_bps = self.order_book_state.spread_bps(&token_id_string).unwrap_or(0);
            if spread_bps < 5000 {
                let msg_count = self.message_counts.get(token_id).copied().unwrap_or(0);
                debug!(
                    "Book: {} | Bid: ${:.4} | Ask: ${:.4} | Spread: {} bps | msgs: {}",
                    &token_id[..token_id.len().min(12)],
                    bid,
                    ask,
                    spread_bps,
                    msg_count
                );
                // Update last log time
                self.last_log_time.insert(token_id.to_string(), now);
            }
        }
    }

    /// Cancel stale orders that have been pending too long
    async fn cleanup_stale_orders(&self) {
        let stale = self.order_tracker.stale_orders(std::time::Duration::from_secs(60));
        if stale.is_empty() {
            return;
        }

        info!("Cleaning up {} stale order(s)", stale.len());
        for order_id in &stale {
            match self.exchange.cancel_order(order_id).await {
                Ok(_) => {
                    self.order_tracker.remove(order_id);
                    info!("Cancelled stale order {}", &order_id[..order_id.len().min(16)]);
                }
                Err(ExchangeError::OrderNotFound(_)) => {
                    // Already filled or cancelled
                    self.order_tracker.remove(order_id);
                    debug!("Stale order {} already gone", &order_id[..order_id.len().min(16)]);
                }
                Err(e) => {
                    warn!("Failed to cancel stale order {}: {}", &order_id[..order_id.len().min(16)], e);
                }
            }
        }
    }

    /// Reconcile ledger balance with exchange REST balance
    async fn reconcile_balance(&self) {
        let exchange_balance = match self.exchange.get_balance().await {
            Ok(b) => b,
            Err(e) => {
                debug!("Balance reconciliation skipped: {}", e);
                return;
            }
        };

        let ledger_balance = self.ledger.cash_snapshot().total;
        let drift = (exchange_balance - ledger_balance).abs();

        if drift > rust_decimal_macros::dec!(1) {
            warn!(
                "Balance drift detected: exchange=${} vs ledger=${} (drift=${})",
                exchange_balance, ledger_balance, drift
            );
        } else {
            debug!(
                "Balance reconciled: exchange=${} ledger=${}",
                exchange_balance, ledger_balance
            );
        }
    }

    /// Graceful shutdown
    async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        // Cancel all outstanding orders on the exchange
        let active_count = self.order_tracker.active_count();
        if active_count > 0 {
            info!("Cancelling {} outstanding order(s) on shutdown...", active_count);
            match self.exchange.cancel_all_orders().await {
                Ok(cancelled) => {
                    info!("Cancelled {} order(s) on shutdown", cancelled.len());
                }
                Err(e) => {
                    error!("Failed to cancel orders on shutdown: {}", e);
                }
            }
        }

        // Get shutdown intents from strategies
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger)
            .with_spot(&self.spot_prices, &self.price_history);
        let shutdown_intents = self.strategy_router.on_shutdown(&ctx);
        if !shutdown_intents.is_empty() {
            info!("Processing {} shutdown intent(s)", shutdown_intents.len());
            self.process_intents(shutdown_intents);
        }

        // Abort WebSocket tasks
        self.market_ws_task.abort();
        self.user_ws_task.abort();

        // Log order tracker status
        self.order_tracker.log_status();

        // Log final stats
        info!(
            "Final stats: {} intents | {} executions | {} fills",
            self.total_intents,
            self.total_executions,
            self.total_fills
        );

        info!("Bot shutdown complete");
    }

    /// Get reference to order book state (for external access)
    pub fn order_book_state(&self) -> &Arc<OrderBookState> {
        &self.order_book_state
    }

    /// Get reference to config
    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Get reference to ledger
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Get reference to market registry
    pub fn market_registry(&self) -> &Arc<MarketPairRegistry> {
        &self.market_registry
    }

    /// Get reference to strategy router
    pub fn strategy_router(&self) -> &Arc<StrategyRouter> {
        &self.strategy_router
    }

    /// Get the WS subscription sender for dynamic market subscriptions
    pub fn ws_subscription_tx(&self) -> &mpsc::UnboundedSender<Vec<String>> {
        &self.ws_subscription_tx
    }

    /// Get reference to kill switch
    pub fn kill_switch(&self) -> &Arc<KillSwitch> {
        &self.kill_switch
    }
}
