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

use crate::api::ApiClient;
use crate::config::{Config, OperatingMode};
use crate::execution::{DualPolicy, ExecutionResult, ExecutionStatus, OrderExecutor, OrderTracker, TrackedOrder};
use crate::kill_switch::KillSwitch;
use crate::ledger::Ledger;
use crate::risk::CircuitBreaker;
use crate::signing::OrderSigner;
use crate::state::OrderBookState;
use crate::strategy::{
    MarketPair, MarketPairRegistry, MathArbStrategy, OrderIntent, StrategyContext, StrategyRouter,
};
use crate::websocket::{MarketMessage, MarketWebSocket, UserMessage, UserWebSocket};
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
        let ledger = Arc::new(Ledger::new(config.max_bet_usd));

        // Set up market pair registry
        let market_registry = Arc::new(MarketPairRegistry::new());
        for pair in market_pairs {
            market_registry.register(pair);
        }

        // Set up strategy router
        let strategy_router = Arc::new(StrategyRouter::new());

        // Register MathArbStrategy with appropriate config
        let arb_config = if config.use_live_test_mode {
            info!("🧪 Using LIVE TEST mode for arb strategy (0.3% min edge, $2-3 positions, maker mode)");
            crate::strategy::MathArbConfig::live_test()
        } else if config.use_maker_mode {
            info!("Using MAKER mode for arb strategy (1% min edge, GTC orders, 0% fees)");
            crate::strategy::MathArbConfig::maker()
        } else {
            info!("Using TAKER mode for arb strategy (3% min edge, FOK orders)");
            crate::strategy::MathArbConfig::taker()
        };
        let math_arb = Arc::new(MathArbStrategy::with_config(market_registry.clone(), arb_config));
        if let Err(e) = strategy_router.register(math_arb) {
            warn!("Failed to register MathArbStrategy: {}", e);
        }

        // Set up circuit breaker for risk management
        let circuit_breaker = Arc::new(CircuitBreaker::new());

        // Create order signer first to get the EOA address
        let order_signer = Arc::new(
            OrderSigner::new(&config.private_key)
                .expect("Failed to create order signer")
        );

        // The signer address (EOA) is what User API credentials are tied to
        let signer_address = order_signer.address();
        info!("EOA Signer address: {}", signer_address);
        info!("Proxy wallet (funder): {}", config.wallet_address);

        // Set up order executor with User API credentials (for L2 auth)
        // IMPORTANT: POLY_ADDRESS header must be the EOA signer address, not the proxy wallet!
        let (api_key, secret_key, passphrase) = if config.has_user_credentials() {
            info!("Using USER credentials for order execution (L2 auth)");
            (
                config.user_api_key.clone().unwrap(),
                config.user_secret_key.clone().unwrap(),
                config.user_passphrase.clone().unwrap(),
            )
        } else {
            warn!("⚠️ No USER credentials configured - using builder credentials (may fail!)");
            warn!("  Run: cargo run --bin derive_creds  to get User API credentials");
            (
                config.api_key.clone(),
                config.secret_key.clone(),
                config.passphrase.clone(),
            )
        };

        // Use signer_address (EOA) for POLY_ADDRESS header, NOT the proxy wallet
        let credentials = crate::api::ApiCredentials::new(
            api_key,
            secret_key,
            passphrase,
            signer_address.clone(), // EOA address that derived the User API credentials
        );

        let api_client = Arc::new(
            ApiClient::new(credentials)
                .expect("Failed to create API client")
        );
        
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
        
        // Note: 15-min crypto markets use neg-risk exchange
        // maker_address = proxy wallet (funder, where funds are)
        let executor = Arc::new(OrderExecutor::new(
            api_client,
            order_signer.clone(),
            policy,
            circuit_breaker.clone(),
            config.wallet_address.clone(), // maker = proxy wallet
            false, // TODO: Detect from market data - using standard exchange for now
        ));

        // Set up order tracker for outstanding orders
        let order_tracker = Arc::new(OrderTracker::new());

        // Set up Market WebSocket for order book data
        let (market_ws_tx, market_ws_rx) = mpsc::unbounded_channel();
        let market_ws = Arc::new(MarketWebSocket::new(token_ids.clone(), market_ws_tx));

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
        let matched_order_id = our_order_ids
            .iter()
            .find(|id| self.order_tracker.contains(id));
        
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
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);

        // Run strategy on_tick() callbacks
        let intents = self.strategy_router.on_tick(&ctx);

        // Process any generated intents
        if !intents.is_empty() {
            self.process_intents(intents);
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
        
        info!(
            "Heartbeat [{}]: {} markets | {} msgs | {:.1} msg/s | {} intents | {} execs | {} fills | {} active | CB: {}",
            mode_str,
            self.order_book_state.num_markets(),
            self.total_messages,
            self.total_messages as f64 / 10.0,  // msgs per second (over 10s window)
            self.total_intents,
            self.total_executions,
            self.total_fills,
            active_orders,
            circuit_status
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
        // Create strategy context
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);

        // Route to strategies
        let intents = self.strategy_router.on_book_update(
            &market_id.to_string(),
            &token_id.to_string(),
            &ctx,
        );

        // Process any generated intents
        if !intents.is_empty() {
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
        info!("🚀 LIVE: Executing {} order(s)...", intents.len());
        
        // Check if intents are grouped (arb legs) and passive (maker orders)
        let has_group = intents.first().and_then(|i| i.group_id.as_ref()).is_some();
        let is_passive = intents.first()
            .map(|i| i.urgency == crate::strategy::Urgency::Passive)
            .unwrap_or(false);
        
        let results = if has_group && is_passive {
            // PARALLEL execution for maker arb legs
            // Both orders go to the book simultaneously, minimizing time window
            // The min order value check in strategy prevents API rejections
            executor.execute_batch(&intents).await
        } else if has_group {
            // SEQUENTIAL execution for taker arb legs (FOK orders)
            // Sequential allows cancellation if second leg fails
            executor.execute_grouped(&intents).await
        } else {
            // Execute as batch (concurrent but independent)
            executor.execute_batch(&intents).await
        };
        
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
                    "🚫 CANCELLED: {} {} @ {} (FOK not filled)",
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

    /// Graceful shutdown
    async fn shutdown(&mut self) {
        info!("Bot shutting down...");

        // Get shutdown intents from strategies
        let ctx = StrategyContext::new(&self.order_book_state, &self.ledger);
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
}
