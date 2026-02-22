//! Momentum Sniper Strategy — single-leg, maker-first momentum sniper
//!
//! Uses Binance WebSocket spot data as an oracle to predict Polymarket
//! 5m/15m crypto market outcomes.
//!
//! ## Strategy Flow
//!
//! 1. Watches Binance BTC/ETH/SOL/XRP spot prices in real-time
//! 2. Computes a conviction score near market close
//! 3. Posts a GTC maker order at 0.93–0.95 on the predicted winning side
//! 4. Runs a sub-200ms cancel/replace loop to avoid adverse selection
//! 5. Falls back to FAK taker if GTC doesn't fill within 2s and conviction is overwhelming
//! 6. Sells winning inventory into strength (maker at 0.98–0.99) with stop-loss on reversal

use crate::api::types::{ConditionId, Side, TokenId};
use crate::ledger::Fill;
use crate::strategy::market_pair::MarketPairRegistry;
use crate::strategy::traits::{OrderAction, OrderIntent, Strategy, StrategyContext, Urgency};
use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// TIMEFRAME
// ============================================================================

/// Market timeframe
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeframe {
    FiveMin,
    FifteenMin,
}

impl Timeframe {
    pub fn label(&self) -> &str {
        match self {
            Timeframe::FiveMin => "5m",
            Timeframe::FifteenMin => "15m",
        }
    }
}

// ============================================================================
// DIRECTION
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Neutral,
}

// ============================================================================
// SNIPER STATE MACHINE
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SniperState {
    /// Waiting for trigger window
    Idle,
    /// Within trigger window, computing conviction
    Monitoring,
    /// GTC maker order posted, cancel/replace loop active
    MakerPosted,
    /// Maker didn't fill in time, posting FAK taker
    TakerFallback,
    /// Position acquired, watching for exit
    InventoryHeld,
    /// Take-profit order posted (maker sell)
    TPPosted,
    /// Stop-loss triggered, dumping via FAK
    StopLoss,
    /// Trade complete
    Completed,
}

// ============================================================================
// MARKET STATE
// ============================================================================

/// Per-market state tracking
#[derive(Debug, Clone)]
pub struct MarketState {
    pub condition_id: String,
    pub asset: String,
    pub timeframe: Timeframe,
    pub state: SniperState,
    /// The GTC order ID (if in MakerPosted state)
    pub maker_order_id: Option<String>,
    /// The token we're betting on (Up or Down)
    pub target_token_id: String,
    /// Entry price (what we paid)
    pub entry_price: Option<Decimal>,
    /// Entry size (shares acquired)
    pub entry_size: Option<Decimal>,
    /// When we entered MakerPosted state (for 2s timeout)
    pub maker_posted_at: Option<Instant>,
    /// Last cancel/replace timestamp (for <200ms loop)
    pub last_cancel_replace: Option<Instant>,
    /// Current conviction score
    pub conviction: Decimal,
    /// Direction (Up/Down)
    pub direction: Direction,
    /// Take-profit order ID (if in TPPosted state)
    pub tp_order_id: Option<String>,
    /// Entry timestamp in Instant (for stop-loss delta calc)
    pub entry_instant: Option<Instant>,
    /// Entry timestamp in unix ms (for price history lookup)
    pub entry_timestamp_ms: Option<i64>,
    /// Price we posted the maker order at (for cancel/replace tracking)
    pub posted_price: Option<Decimal>,
    /// Whether we've sent a cancel and are awaiting confirmation
    pub pending_cancel: bool,
    /// Market close time (unix timestamp)
    pub close_time: Option<i64>,
}

impl MarketState {
    pub fn new(condition_id: String, asset: String, timeframe: Timeframe) -> Self {
        Self {
            condition_id,
            asset,
            timeframe,
            state: SniperState::Idle,
            maker_order_id: None,
            target_token_id: String::new(),
            entry_price: None,
            entry_size: None,
            maker_posted_at: None,
            last_cancel_replace: None,
            conviction: Decimal::ZERO,
            direction: Direction::Neutral,
            tp_order_id: None,
            entry_instant: None,
            entry_timestamp_ms: None,
            posted_price: None,
            pending_cancel: false,
            close_time: None,
        }
    }
}

// ============================================================================
// CONVICTION ENGINE
// ============================================================================

/// Computes conviction score from multi-timeframe momentum
pub struct ConvictionEngine;

impl ConvictionEngine {
    /// Compute conviction score for an asset
    ///
    /// Returns (score 0.0–1.0, direction)
    pub fn compute(
        ctx: &StrategyContext,
        asset: &str,
        config: &MomentumConfig,
    ) -> (Decimal, Direction) {
        let history = match ctx.price_history {
            Some(h) => h,
            None => return (Decimal::ZERO, Direction::Neutral),
        };

        // Ensure we have enough data
        let min_observations = 10;
        let longest_window = config.lookback_windows_ms.last().copied().unwrap_or(300_000);
        if history.observation_count(asset, longest_window) < min_observations {
            return (Decimal::ZERO, Direction::Neutral);
        }

        // Compute weighted delta across lookback windows
        let mut weighted_delta = Decimal::ZERO;
        let mut all_positive = true;
        let mut all_negative = true;
        let mut has_data = true;

        for (i, &window_ms) in config.lookback_windows_ms.iter().enumerate() {
            let delta = match history.price_change_pct(asset, window_ms) {
                Some(d) => d,
                None => {
                    has_data = false;
                    break;
                }
            };

            let weight = config.lookback_weights.get(i).copied().unwrap_or(Decimal::ZERO);
            weighted_delta += weight * delta;

            if delta <= Decimal::ZERO {
                all_positive = false;
            }
            if delta >= Decimal::ZERO {
                all_negative = false;
            }
        }

        if !has_data {
            return (Decimal::ZERO, Direction::Neutral);
        }

        // Consistency bonus: all windows agree on direction
        let consistency_multiplier = if all_positive || all_negative {
            dec!(1.3)
        } else {
            dec!(0.7)
        };

        // Volatility adjustment
        let vol_penalty = match history.volatility(asset, 60_000) {
            Some(vol) if vol > Decimal::ZERO => {
                // Baseline vol roughly 0.0003 for BTC in 1-min windows
                let baseline = dec!(0.0003);
                let ratio = vol / baseline;
                // Clamp between 0.5 and 1.5
                ratio.max(dec!(0.5)).min(dec!(1.5))
            }
            _ => dec!(1.0), // No vol data, no penalty
        };

        // Raw score
        let abs_delta = weighted_delta.abs();
        let raw_score = abs_delta * consistency_multiplier / vol_penalty;

        // Calibration: 0.001 (0.1% move) maps to ~0.7 conviction
        let calibration = dec!(0.0014);
        let conviction = (raw_score / calibration).min(dec!(1.0));

        let direction = if weighted_delta > Decimal::ZERO {
            Direction::Up
        } else if weighted_delta < Decimal::ZERO {
            Direction::Down
        } else {
            Direction::Neutral
        };

        (conviction, direction)
    }
}

// ============================================================================
// MOMENTUM CONFIG
// ============================================================================

#[derive(Debug, Clone)]
pub struct MomentumConfig {
    // === Timeframe Parameters ===
    /// Seconds before close to start monitoring (trigger window entry)
    pub trigger_window_secs: i64,
    /// Seconds before close to fire the order (trigger point)
    pub trigger_fire_secs: i64,

    // === Conviction Scoring ===
    /// Minimum conviction score to act (0.0 – 1.0)
    pub min_conviction: Decimal,
    /// "Overwhelming" conviction threshold for taker fallback
    pub overwhelming_conviction: Decimal,
    /// Lookback windows for multi-timeframe momentum (in milliseconds)
    pub lookback_windows_ms: Vec<i64>,
    /// Weights for each lookback window (must sum to 1.0)
    pub lookback_weights: Vec<Decimal>,

    // === Maker Entry ===
    /// Minimum acceptable entry price (skip if book price is below this)
    pub min_entry_price: Decimal,
    /// Maximum time to wait for maker fill before taker fallback (ms)
    pub maker_fill_timeout_ms: u64,
    /// Cancel/replace loop interval (ms)
    pub cancel_replace_interval_ms: u64,

    // === Taker Fallback ===
    /// Maximum price willing to pay as taker (after fees)
    pub max_taker_price: Decimal,

    // === Inventory Exit ===
    /// Take-profit spread above entry price (e.g., 0.04 = 4 cents above entry)
    pub tp_spread: Decimal,
    /// Aggressive take-profit spread (wider margin if bid is very high)
    pub aggressive_tp_spread: Decimal,
    /// Stop-loss: minimum spot delta reversal to trigger emergency dump (Binance)
    pub stop_loss_reversal_pct: Decimal,
    /// Book-based stop-loss: dump if best_bid drops this far below entry
    pub book_stop_loss_spread: Decimal,
    /// Force exit this many seconds before market resolution
    pub max_hold_before_close_secs: u64,

    // === Sizing ===
    pub max_size_per_trade: Decimal,
    pub max_total_exposure: Decimal,

    // === Assets ===
    pub assets: Vec<String>,
}

impl MomentumConfig {
    /// Conservative preset for 5-minute markets
    pub fn preset_5m() -> Self {
        Self {
            trigger_window_secs: 15,
            trigger_fire_secs: 10,
            min_conviction: dec!(0.65),
            overwhelming_conviction: dec!(0.85),
            lookback_windows_ms: vec![60_000, 180_000, 300_000],
            lookback_weights: vec![dec!(0.5), dec!(0.3), dec!(0.2)],
            min_entry_price: dec!(0.10),
            maker_fill_timeout_ms: 2000,
            cancel_replace_interval_ms: 150,
            max_taker_price: dec!(0.96),
            tp_spread: dec!(0.04),
            aggressive_tp_spread: dec!(0.06),
            stop_loss_reversal_pct: dec!(-0.003),
            book_stop_loss_spread: dec!(0.03),
            max_hold_before_close_secs: 30,
            max_size_per_trade: dec!(15),
            max_total_exposure: dec!(40),
            assets: vec![
                "btc".to_string(),
                "eth".to_string(),
                "sol".to_string(),
                "xrp".to_string(),
            ],
        }
    }

    /// Conservative preset for 15-minute markets
    ///
    /// Key differences from 5m:
    /// - Wider trigger window (30s) and fire point (15s) — more time for price to settle
    /// - Longer lookback windows (3m/10m/15m) — captures the full 15m trend
    /// - Longer maker fill timeout (3s) — 15m markets move slower
    /// - Tighter TP spread (0.03) — more realistic fill probability before resolution
    /// - Wider stop-loss (-0.5%) — avoids noise-triggered dumps on longer timeframe
    /// - Wider book SL (0.04) — same reasoning
    /// - Longer hold before close (45s) — more time to exit
    pub fn preset_15m() -> Self {
        Self {
            trigger_window_secs: 30,
            trigger_fire_secs: 15,
            min_conviction: dec!(0.65),
            overwhelming_conviction: dec!(0.85),
            lookback_windows_ms: vec![180_000, 600_000, 900_000],
            lookback_weights: vec![dec!(0.5), dec!(0.3), dec!(0.2)],
            min_entry_price: dec!(0.10),
            maker_fill_timeout_ms: 3000,
            cancel_replace_interval_ms: 150,
            max_taker_price: dec!(0.96),
            tp_spread: dec!(0.03),
            aggressive_tp_spread: dec!(0.05),
            stop_loss_reversal_pct: dec!(-0.005),
            book_stop_loss_spread: dec!(0.04),
            max_hold_before_close_secs: 45,
            max_size_per_trade: dec!(15),
            max_total_exposure: dec!(40),
            assets: vec![
                "btc".to_string(),
                "eth".to_string(),
                "sol".to_string(),
                "xrp".to_string(),
            ],
        }
    }

    /// Default live test configuration ($50 max exposure)
    pub fn default_live_test() -> Self {
        Self::from_env_with_defaults(Self::preset_5m())
    }

    /// Load overrides from environment variables
    fn from_env_with_defaults(mut config: Self) -> Self {
        if let Ok(v) = std::env::var("MOMENTUM_MIN_CONVICTION") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.min_conviction = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_MIN_ENTRY_PRICE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.min_entry_price = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_MAKER_FILL_TIMEOUT_MS") {
            if let Ok(d) = v.parse::<u64>() {
                config.maker_fill_timeout_ms = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_TP_SPREAD") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.tp_spread = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_BOOK_SL_SPREAD") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.book_stop_loss_spread = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_STOP_LOSS_PCT") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.stop_loss_reversal_pct = -d.abs();
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_MAX_EXPOSURE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.max_total_exposure = d;
            }
        }
        if let Ok(v) = std::env::var("MOMENTUM_MAX_SIZE_PER_TRADE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.max_size_per_trade = d;
            }
        }
        config
    }
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self::preset_5m()
    }
}

// ============================================================================
// MOMENTUM STRATEGY
// ============================================================================

/// Single-leg momentum sniper for 5m/15m crypto markets
pub struct MomentumStrategy {
    /// Configuration (using 5m defaults; timeframe-specific params applied per-market)
    config: MomentumConfig,
    /// Market pair registry
    registry: Arc<MarketPairRegistry>,
    /// Per-market state machines
    markets: DashMap<ConditionId, MarketState>,
    /// Enabled flag
    enabled: bool,
}

impl MomentumStrategy {
    /// Create with config
    pub fn new(registry: Arc<MarketPairRegistry>, config: MomentumConfig) -> Self {
        Self {
            config,
            registry,
            markets: DashMap::new(),
            enabled: true,
        }
    }

    /// Extract asset name from event slug (e.g. "btc-updown-5m-1740000000" -> "btc")
    fn extract_asset(slug: &str) -> Option<String> {
        let first_part = slug.split('-').next()?;
        let asset = first_part.to_lowercase();
        match asset.as_str() {
            "btc" | "bitcoin" => Some("btc".to_string()),
            "eth" | "ethereum" => Some("eth".to_string()),
            "sol" | "solana" => Some("sol".to_string()),
            "xrp" => Some("xrp".to_string()),
            _ => None,
        }
    }

    /// Check if this market is momentum-eligible (5m or 15m)
    fn is_momentum_eligible(slug: &str) -> bool {
        slug.contains("-5m-") || slug.contains("-15m-")
    }

    /// Detect timeframe from slug
    fn detect_timeframe(slug: &str) -> Option<Timeframe> {
        if slug.contains("-5m-") {
            Some(Timeframe::FiveMin)
        } else if slug.contains("-15m-") {
            Some(Timeframe::FifteenMin)
        } else {
            None
        }
    }

    /// Get timeframe-specific config parameters
    fn config_for_timeframe(&self, tf: Timeframe) -> MomentumConfig {
        match tf {
            Timeframe::FiveMin => {
                // Use base config (already 5m defaults)
                self.config.clone()
            }
            Timeframe::FifteenMin => {
                // Apply all 15m-specific overrides from preset
                let preset = MomentumConfig::preset_15m();
                let mut c = self.config.clone();
                c.trigger_window_secs = preset.trigger_window_secs;       // 30s
                c.trigger_fire_secs = preset.trigger_fire_secs;           // 15s
                c.lookback_windows_ms = preset.lookback_windows_ms;       // [180k, 600k, 900k]
                c.maker_fill_timeout_ms = preset.maker_fill_timeout_ms;   // 3000ms (slower markets)
                c.tp_spread = preset.tp_spread;                           // 0.03 (tighter for 15m)
                c.aggressive_tp_spread = preset.aggressive_tp_spread;     // 0.05
                c.stop_loss_reversal_pct = preset.stop_loss_reversal_pct; // -0.5% (wider to avoid noise)
                c.book_stop_loss_spread = preset.book_stop_loss_spread;   // 0.04 (wider for 15m)
                c.max_hold_before_close_secs = preset.max_hold_before_close_secs; // 45s
                c
            }
        }
    }

    /// Clean up completed/stale market states (older than 30 minutes)
    fn cleanup_stale(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(1800);
        self.markets.retain(|_, ms| {
            // Keep if not completed, or if completed recently
            ms.state != SniperState::Completed
                || ms.maker_posted_at.map_or(true, |t| t > cutoff)
        });
    }

    /// Process Idle → Monitoring transition
    fn process_idle(
        &self,
        ms: &mut MarketState,
        secs_until_close: i64,
        tf_config: &MomentumConfig,
    ) {
        if secs_until_close <= tf_config.trigger_window_secs && secs_until_close > 0 {
            ms.state = SniperState::Monitoring;
            debug!(
                "MomentumSniper: {} {} → Monitoring ({}s to close)",
                ms.asset,
                ms.timeframe.label(),
                secs_until_close
            );
        }
    }

    /// Process Monitoring → MakerPosted transition
    fn process_monitoring(
        &self,
        ms: &mut MarketState,
        secs_until_close: i64,
        ctx: &StrategyContext,
        tf_config: &MomentumConfig,
    ) -> Vec<OrderIntent> {
        // Compute conviction
        let (conviction, direction) = ConvictionEngine::compute(ctx, &ms.asset, tf_config);
        ms.conviction = conviction;
        ms.direction = direction;

        debug!(
            "MomentumSniper: {} {} conviction={:.3} dir={:?} ({}s to close)",
            ms.asset,
            ms.timeframe.label(),
            conviction,
            direction,
            secs_until_close
        );

        // Check if we should fire
        if secs_until_close > tf_config.trigger_fire_secs {
            return Vec::new(); // Not yet in fire window
        }

        if conviction < tf_config.min_conviction {
            debug!(
                "MomentumSniper: {} conviction {:.3} < {:.3} threshold, staying in Monitoring",
                ms.asset, conviction, tf_config.min_conviction
            );
            return Vec::new();
        }

        if direction == Direction::Neutral {
            return Vec::new();
        }

        // Exposure check
        if ctx.total_exposure() >= tf_config.max_total_exposure {
            debug!(
                "MomentumSniper: max exposure ${} reached, skipping",
                tf_config.max_total_exposure
            );
            ms.state = SniperState::Completed;
            return Vec::new();
        }

        // Look up the winning token
        let pair = match self.registry.get_by_condition(&ms.condition_id) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let winning_token = match direction {
            Direction::Up => pair.up_token_id().clone(),
            Direction::Down => pair.down_token_id().clone(),
            Direction::Neutral => return Vec::new(),
        };

        // Dynamic pricing from order book
        let best_bid = ctx.best_bid(&winning_token);
        let best_ask = ctx.best_ask(&winning_token);

        let maker_price = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => {
                // Post one tick above best bid (top of maker queue)
                let target = bid + dec!(0.01);
                if target >= ask {
                    // Would cross — post one tick below best ask instead
                    let safe_price = ask - dec!(0.01);
                    if safe_price < dec!(0.01) {
                        debug!("MomentumSniper: {} spread too tight, skipping", ms.asset);
                        return Vec::new();
                    }
                    safe_price
                } else {
                    target
                }
            }
            (Some(bid), None) => bid + dec!(0.01),
            (None, Some(ask)) => ask - dec!(0.01),
            (None, None) => {
                debug!("MomentumSniper: {} no book data, skipping", ms.asset);
                return Vec::new();
            }
        };

        // Sanity: don't buy below min entry price or above max taker price
        if maker_price < tf_config.min_entry_price {
            debug!(
                "MomentumSniper: {} price {} below min_entry_price {}, skipping",
                ms.asset, maker_price, tf_config.min_entry_price
            );
            return Vec::new();
        }
        let maker_price = maker_price.min(tf_config.max_taker_price);

        // Sizing
        let max_affordable = (ctx.available_cash() / maker_price).floor();
        let remaining_exposure = tf_config.max_total_exposure - ctx.total_exposure();
        let max_from_exposure = (remaining_exposure / maker_price).floor();
        let trade_size = tf_config
            .max_size_per_trade
            .min(max_affordable)
            .min(max_from_exposure)
            .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero);

        // Need at least $1 order value
        let min_shares = (dec!(1) / maker_price).ceil();
        if trade_size < min_shares {
            debug!("MomentumSniper: insufficient cash for trade");
            ms.state = SniperState::Completed;
            return Vec::new();
        }

        let dir_label = match direction {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
            Direction::Neutral => "?",
        };

        info!(
            "{} Momentum: {} {} (conv={:.3}, {}s to close) → MAKER BUY {} @ {} x {} (bid={:?} ask={:?})",
            ms.timeframe.label(),
            ms.asset,
            dir_label,
            conviction,
            secs_until_close,
            &winning_token[..winning_token.len().min(12)],
            maker_price,
            trade_size,
            best_bid,
            best_ask,
        );

        // Transition to MakerPosted
        ms.target_token_id = winning_token.clone();
        ms.state = SniperState::MakerPosted;
        ms.maker_posted_at = Some(Instant::now());
        ms.posted_price = Some(maker_price);

        let intent = OrderIntent::new(
            ms.condition_id.clone(),
            winning_token,
            Side::Buy,
            maker_price,
            trade_size,
            Urgency::Passive, // GTC maker
            format!(
                "{} momentum {} {} (conv={:.2})",
                ms.timeframe.label(),
                ms.asset,
                dir_label,
                conviction,
            ),
            "MomentumSniper",
        )
        .with_fee_rate(pair.fee_rate_bps)
        .with_priority(60);

        vec![intent]
    }

    /// Process InventoryHeld — check for take-profit, stop-loss, and emergency exits
    fn process_inventory_held(
        &self,
        ms: &mut MarketState,
        ctx: &StrategyContext,
        tf_config: &MomentumConfig,
    ) -> Vec<OrderIntent> {
        let pair = match self.registry.get_by_condition(&ms.condition_id) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
        let entry_size = ms.entry_size.unwrap_or(Decimal::ZERO);
        if entry_price <= Decimal::ZERO || entry_size <= Decimal::ZERO {
            return Vec::new();
        }

        // === EMERGENCY EXIT: Time-based (force exit before market close) ===
        if let Some(close_time) = ms.close_time {
            let secs_until_close = close_time - ctx.utc_now.timestamp();
            if secs_until_close < tf_config.max_hold_before_close_secs as i64 {
                let sell_price = ctx
                    .best_bid(&ms.target_token_id)
                    .unwrap_or(dec!(0.50));

                warn!(
                    "EMERGENCY EXIT: {} {} {}s until close → FAK SELL {} @ {}",
                    ms.asset,
                    ms.timeframe.label(),
                    secs_until_close,
                    entry_size,
                    sell_price,
                );

                ms.state = SniperState::StopLoss;

                return vec![OrderIntent::new(
                    ms.condition_id.clone(),
                    ms.target_token_id.clone(),
                    Side::Sell,
                    sell_price,
                    entry_size,
                    Urgency::Normal, // FAK
                    format!("emergency-exit {} {}", ms.asset, ms.timeframe.label()),
                    "MomentumSniper",
                )
                .with_fee_rate(pair.fee_rate_bps)
                .with_priority(95)]; // Highest priority
            }
        }

        // === STOP-LOSS 1: Book-based (best_bid dropped below entry - spread) ===
        let book_sl_price = entry_price - tf_config.book_stop_loss_spread;
        if let Some(best_bid) = ctx.best_bid(&ms.target_token_id) {
            if best_bid < book_sl_price {
                let sell_price = best_bid; // Dump at current best bid

                warn!(
                    "BOOK STOP-LOSS: {} {} best_bid={} < sl_price={} → FAK SELL {} @ {}",
                    ms.asset,
                    ms.timeframe.label(),
                    best_bid,
                    book_sl_price,
                    entry_size,
                    sell_price,
                );

                ms.state = SniperState::StopLoss;

                return vec![OrderIntent::new(
                    ms.condition_id.clone(),
                    ms.target_token_id.clone(),
                    Side::Sell,
                    sell_price,
                    entry_size,
                    Urgency::Normal, // FAK
                    format!("book-sl {} {}", ms.asset, ms.timeframe.label()),
                    "MomentumSniper",
                )
                .with_fee_rate(pair.fee_rate_bps)
                .with_priority(90)];
            }
        }

        // === STOP-LOSS 2: Binance delta reversal (crash protection) ===
        if let Some(entry_ts_ms) = ms.entry_timestamp_ms {
            let now_ms = ctx.utc_now.timestamp_millis();
            let ms_since_entry = now_ms - entry_ts_ms;
            if let Some(delta) = ctx.spot_change_pct(&ms.asset, ms_since_entry) {
                // Check direction-adjusted reversal
                let reversal = match ms.direction {
                    Direction::Up => delta, // If we bought UP, negative delta = reversal
                    Direction::Down => -delta, // If we bought DOWN, positive delta = reversal
                    Direction::Neutral => Decimal::ZERO,
                };

                if reversal < tf_config.stop_loss_reversal_pct {
                    let sell_price = ctx
                        .best_bid(&ms.target_token_id)
                        .map(|b| b - dec!(0.01))
                        .unwrap_or(dec!(0.50));

                    warn!(
                        "BINANCE STOP-LOSS: {} {} reversal={:.4}% (threshold={:.3}%) → FAK SELL {} @ {}",
                        ms.asset,
                        ms.timeframe.label(),
                        reversal * dec!(100),
                        tf_config.stop_loss_reversal_pct * dec!(100),
                        entry_size,
                        sell_price,
                    );

                    ms.state = SniperState::StopLoss;

                    return vec![OrderIntent::new(
                        ms.condition_id.clone(),
                        ms.target_token_id.clone(),
                        Side::Sell,
                        sell_price,
                        entry_size,
                        Urgency::Normal, // FAK
                        format!("binance-sl {} {}", ms.asset, ms.timeframe.label()),
                        "MomentumSniper",
                    )
                    .with_fee_rate(pair.fee_rate_bps)
                    .with_priority(90)];
                }
            }
        }

        // === TAKE-PROFIT: Post GTC SELL at entry + tp_spread immediately ===
        let tp_price = (entry_price + tf_config.tp_spread).min(dec!(0.99));

        // Check if book bid has risen enough for aggressive TP
        let final_tp_price = if let Some(best_bid) = ctx.best_bid(&ms.target_token_id) {
            let aggressive_tp = (entry_price + tf_config.aggressive_tp_spread).min(dec!(0.99));
            if best_bid >= aggressive_tp {
                aggressive_tp // Bid is very high, take aggressive TP
            } else {
                tp_price // Standard TP
            }
        } else {
            tp_price
        };

        info!(
            "TAKE-PROFIT: {} {} entry={} → MAKER SELL {} @ {} (tp_spread={})",
            ms.asset,
            ms.timeframe.label(),
            entry_price,
            entry_size,
            final_tp_price,
            tf_config.tp_spread,
        );

        ms.state = SniperState::TPPosted;

        vec![OrderIntent::new(
            ms.condition_id.clone(),
            ms.target_token_id.clone(),
            Side::Sell,
            final_tp_price,
            entry_size,
            Urgency::Passive, // GTC maker sell
            format!("take-profit {} {}", ms.asset, ms.timeframe.label()),
            "MomentumSniper",
        )
        .with_fee_rate(pair.fee_rate_bps)
        .with_priority(70)]
    }
}

impl Strategy for MomentumStrategy {
    fn name(&self) -> &str {
        "MomentumSniper"
    }

    fn priority(&self) -> u8 {
        60
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        // Subscribe to all markets — we filter by slug in on_tick
        Vec::new()
    }

    fn on_book_update(
        &self,
        _market_id: &ConditionId,
        _token_id: &TokenId,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        // Momentum strategy acts on tick, not on book updates
        Vec::new()
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Need spot prices to function
        if ctx.spot_prices.is_none() || ctx.price_history.is_none() {
            return Vec::new();
        }

        // Periodic cleanup
        self.cleanup_stale();

        let now_unix = ctx.utc_now.timestamp();
        let mut intents = Vec::new();

        // Iterate all registered momentum-eligible markets
        let pairs = self.registry.filter(|pair| {
            Self::is_momentum_eligible(&pair.event_slug) && pair.close_time.is_some()
        });

        for pair in pairs {
            let condition_id = &pair.condition_id;
            let close_time = match pair.close_time {
                Some(t) => t,
                None => continue,
            };

            let secs_until_close = close_time - now_unix;

            // Skip markets that already closed
            if secs_until_close < -5 {
                // Cleanup if completed
                if let Some(mut ms) = self.markets.get_mut(condition_id) {
                    if ms.state == SniperState::StopLoss || ms.state == SniperState::TPPosted {
                        ms.state = SniperState::Completed;
                    }
                }
                continue;
            }

            let slug = &pair.event_slug;
            let asset = match Self::extract_asset(slug) {
                Some(a) => a,
                None => continue,
            };

            if !self.config.assets.contains(&asset) {
                continue;
            }

            let timeframe = match Self::detect_timeframe(slug) {
                Some(tf) => tf,
                None => continue,
            };

            let tf_config = self.config_for_timeframe(timeframe);

            // Get or create market state
            let mut ms = self
                .markets
                .entry(condition_id.clone())
                .or_insert_with(|| MarketState::new(condition_id.clone(), asset.clone(), timeframe));

            // Store close_time for use in inventory management
            if ms.close_time.is_none() {
                ms.close_time = Some(close_time);
            }

            match ms.state {
                SniperState::Idle => {
                    self.process_idle(&mut ms, secs_until_close, &tf_config);
                }
                SniperState::Monitoring => {
                    let new_intents =
                        self.process_monitoring(&mut ms, secs_until_close, ctx, &tf_config);
                    intents.extend(new_intents);
                }
                SniperState::InventoryHeld => {
                    let new_intents =
                        self.process_inventory_held(&mut ms, ctx, &tf_config);
                    intents.extend(new_intents);
                }
                SniperState::Completed | SniperState::StopLoss => {
                    // Nothing to do
                }
                SniperState::MakerPosted | SniperState::TakerFallback => {
                    // Handled in on_order_management
                }
                SniperState::TPPosted => {
                    // TP order is out, wait for fill or expiry
                    // If market closed, let PositionRedeemer handle it
                }
            }
        }

        intents
    }

    fn on_order_management(&self, ctx: &StrategyContext) -> Vec<OrderAction> {
        let mut actions = Vec::new();

        // Iterate all active market states
        for mut entry in self.markets.iter_mut() {
            let ms = entry.value_mut();

            match ms.state {
                SniperState::MakerPosted => {
                    let posted_at = match ms.maker_posted_at {
                        Some(t) => t,
                        None => {
                            ms.state = SniperState::Completed;
                            continue;
                        }
                    };

                    let elapsed_ms = posted_at.elapsed().as_millis() as u64;

                    // Timeout check
                    if elapsed_ms > self.config.maker_fill_timeout_ms {
                        // Cancel the maker order
                        if let Some(ref order_id) = ms.maker_order_id {
                            actions.push(OrderAction::Cancel {
                                order_id: order_id.clone(),
                            });
                        }

                        if ms.conviction >= self.config.overwhelming_conviction {
                            // Taker fallback
                            info!(
                                "MomentumSniper: {} maker timeout ({}ms), conviction {:.3} → TAKER FALLBACK",
                                ms.asset, elapsed_ms, ms.conviction
                            );

                            let pair = match self.registry.get_by_condition(&ms.condition_id) {
                                Some(p) => p,
                                None => {
                                    ms.state = SniperState::Completed;
                                    continue;
                                }
                            };

                            // Check best ask is within our taker limit
                            let best_ask = ctx.best_ask(&ms.target_token_id);
                            let taker_price = match best_ask {
                                Some(ask) if ask <= self.config.max_taker_price => ask,
                                Some(ask) => {
                                    info!(
                                        "MomentumSniper: {} best ask {} > max taker {}, abandoning",
                                        ms.asset, ask, self.config.max_taker_price
                                    );
                                    ms.state = SniperState::Completed;
                                    continue;
                                }
                                None => {
                                    ms.state = SniperState::Completed;
                                    continue;
                                }
                            };

                            // Size for taker (recompute)
                            let max_affordable =
                                (ctx.available_cash() / taker_price).floor();
                            let remaining_exposure =
                                self.config.max_total_exposure - ctx.total_exposure();
                            let max_from_exposure =
                                (remaining_exposure / taker_price).floor();
                            let trade_size = self
                                .config
                                .max_size_per_trade
                                .min(max_affordable)
                                .min(max_from_exposure)
                                .round_dp_with_strategy(
                                    2,
                                    rust_decimal::RoundingStrategy::ToZero,
                                );

                            let min_shares = (dec!(1) / taker_price).ceil();
                            if trade_size < min_shares {
                                ms.state = SniperState::Completed;
                                continue;
                            }

                            ms.state = SniperState::TakerFallback;

                            let intent = OrderIntent::new(
                                ms.condition_id.clone(),
                                ms.target_token_id.clone(),
                                Side::Buy,
                                taker_price,
                                trade_size,
                                Urgency::Normal, // FAK
                                format!(
                                    "taker fallback {} {} (conv={:.2})",
                                    ms.asset,
                                    ms.timeframe.label(),
                                    ms.conviction,
                                ),
                                "MomentumSniper",
                            )
                            .with_fee_rate(pair.fee_rate_bps)
                            .with_priority(65);

                            actions.push(OrderAction::PostTakerFallback { intent });
                        } else {
                            info!(
                                "MomentumSniper: {} maker timeout ({}ms), conviction {:.3} too low for taker fallback, abandoning",
                                ms.asset, elapsed_ms, ms.conviction
                            );
                            ms.state = SniperState::Completed;
                        }
                        continue;
                    }

                    // Cancel/replace loop: re-derive optimal price from book each tick
                    let should_replace = ms
                        .last_cancel_replace
                        .map(|t| t.elapsed().as_millis() as u64 >= self.config.cancel_replace_interval_ms)
                        .unwrap_or(true);

                    if should_replace {
                        if let Some(ref order_id) = ms.maker_order_id {
                            let best_bid = ctx.best_bid(&ms.target_token_id);
                            let best_ask = ctx.best_ask(&ms.target_token_id);

                            if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
                                // Compute optimal maker price (same logic as initial entry)
                                let target = bid + dec!(0.01);
                                let optimal_price = if target >= ask {
                                    ask - dec!(0.01)
                                } else {
                                    target
                                };

                                if let Some(posted) = ms.posted_price {
                                    if optimal_price != posted && optimal_price >= dec!(0.01) {
                                        debug!(
                                            "MomentumSniper: {} cancel/replace {} → {} (bid={} ask={})",
                                            ms.asset, posted, optimal_price, bid, ask
                                        );

                                        let pair = match self.registry.get_by_condition(&ms.condition_id) {
                                            Some(p) => p,
                                            None => continue,
                                        };

                                        let trade_size = self.config.max_size_per_trade;

                                        let new_intent = OrderIntent::new(
                                            ms.condition_id.clone(),
                                            ms.target_token_id.clone(),
                                            Side::Buy,
                                            optimal_price,
                                            trade_size,
                                            Urgency::Passive,
                                            format!("cancel/replace {} {}", ms.asset, ms.timeframe.label()),
                                            "MomentumSniper",
                                        )
                                        .with_fee_rate(pair.fee_rate_bps)
                                        .with_priority(60);

                                        ms.posted_price = Some(optimal_price);

                                        actions.push(OrderAction::Replace {
                                            old_order_id: order_id.clone(),
                                            new_intent,
                                        });
                                    }
                                }
                            }
                        }
                        ms.last_cancel_replace = Some(Instant::now());
                    }
                }
                _ => {} // Other states handled in on_tick
            }
        }

        actions
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        // Find which market state this fill belongs to
        for mut entry in self.markets.iter_mut() {
            let ms = entry.value_mut();

            if ms.target_token_id != fill.token_id {
                continue;
            }

            match ms.state {
                SniperState::MakerPosted | SniperState::TakerFallback => {
                    if fill.side == Side::Buy {
                        info!(
                            "MomentumSniper: {} {} FILLED {} @ {} → InventoryHeld",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                        );
                        ms.entry_price = Some(fill.price);
                        ms.entry_size = Some(fill.size);
                        ms.entry_instant = Some(Instant::now());
                        ms.entry_timestamp_ms = Some(chrono::Utc::now().timestamp_millis());
                        ms.state = SniperState::InventoryHeld;
                    }
                }
                SniperState::Completed => {
                    // Late fill recovery: a fill arrived after state timed out
                    if fill.side == Side::Buy {
                        warn!(
                            "MomentumSniper: {} {} LATE FILL after timeout: {} @ {} → recovering to InventoryHeld",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                        );
                        ms.entry_price = Some(fill.price);
                        ms.entry_size = Some(fill.size);
                        ms.entry_instant = Some(Instant::now());
                        ms.entry_timestamp_ms = Some(chrono::Utc::now().timestamp_millis());
                        ms.state = SniperState::InventoryHeld;
                    }
                }
                SniperState::TPPosted | SniperState::StopLoss => {
                    if fill.side == Side::Sell {
                        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let pnl = (fill.price - entry_price) * fill.size - fill.fee;
                        info!(
                            "MomentumSniper: {} {} EXIT FILLED {} @ {} (entry={}, PnL=${:.4})",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            entry_price,
                            pnl,
                        );
                        ms.state = SniperState::Completed;
                    }
                }
                _ => {}
            }
        }

        Vec::new()
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::state::{OrderBookState, PriceHistory, SpotPriceState, SpotPriceUpdate};

    #[test]
    fn test_extract_asset() {
        assert_eq!(
            MomentumStrategy::extract_asset("btc-updown-5m-1740000000"),
            Some("btc".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("eth-updown-15m-1740000000"),
            Some("eth".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("sol-updown-5m-1740000000"),
            Some("sol".to_string())
        );
        assert_eq!(
            MomentumStrategy::extract_asset("unknown-updown-5m-1740000000"),
            None
        );
    }

    #[test]
    fn test_is_momentum_eligible() {
        assert!(MomentumStrategy::is_momentum_eligible(
            "btc-updown-5m-1740000000"
        ));
        assert!(MomentumStrategy::is_momentum_eligible(
            "btc-updown-15m-1740000000"
        ));
        assert!(!MomentumStrategy::is_momentum_eligible(
            "btc-updown-1h-1740000000"
        ));
        assert!(!MomentumStrategy::is_momentum_eligible("some-other-slug"));
    }

    #[test]
    fn test_detect_timeframe() {
        assert_eq!(
            MomentumStrategy::detect_timeframe("btc-updown-5m-1740000000"),
            Some(Timeframe::FiveMin)
        );
        assert_eq!(
            MomentumStrategy::detect_timeframe("eth-updown-15m-1740000000"),
            Some(Timeframe::FifteenMin)
        );
        assert_eq!(
            MomentumStrategy::detect_timeframe("btc-updown-1h-1740000000"),
            None
        );
    }

    #[test]
    fn test_conviction_engine_up_move() {
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Simulate BTC going UP consistently over 5 minutes
        // Record prices every second from 5min ago to now
        for i in 0..300 {
            let ts = now_ms - (300_000 - i * 1000);
            let price = dec!(50000) + Decimal::from(i) * dec!(1);
            history.record("btc", ts, price);
        }
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50300),
            timestamp_ms: now_ms,
        });

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let config = MomentumConfig::preset_5m();
        let (conviction, direction) = ConvictionEngine::compute(&ctx, "btc", &config);

        assert!(conviction > dec!(0.5), "Conviction should be significant: {}", conviction);
        assert_eq!(direction, Direction::Up);
    }

    #[test]
    fn test_conviction_engine_no_data() {
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let config = MomentumConfig::preset_5m();
        let (conviction, direction) = ConvictionEngine::compute(&ctx, "btc", &config);

        assert_eq!(conviction, Decimal::ZERO);
        assert_eq!(direction, Direction::Neutral);
    }

    #[test]
    fn test_state_machine_idle_to_monitoring() {
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        let pair = crate::strategy::MarketPair::new_up_down(
            "0x5m_test".to_string(),
            "up_token_123".to_string(),
            "down_token_456".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 10); // 10 seconds to close

        registry.register(pair);

        let config = MomentumConfig::preset_5m();
        let strategy = MomentumStrategy::new(registry, config);

        // Set up minimal spot data
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Add enough data points for conviction engine
        for i in 0..300 {
            let ts = now_ms - (300_000 - i * 1000);
            history.record("btc", ts, dec!(50000) + Decimal::from(i));
        }
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50300),
            timestamp_ms: now_ms,
        });

        // Set up order book with ask at 0.90
        let books = OrderBookState::new();
        books.update_book(
            "up_token_123".to_string(),
            "0x5m_test".to_string(),
            vec![],
            vec![crate::api::types::PriceLevel {
                price: "0.90".to_string(),
                size: "100".to_string(),
            }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        // First tick should transition to Monitoring, then possibly to MakerPosted
        let _intents = strategy.on_tick(&ctx);

        // Check that the strategy created a market state
        assert!(!strategy.markets.is_empty(), "Should have created a market state");

        // Check the state
        let ms = strategy.markets.get("0x5m_test").unwrap();
        // Should be Monitoring or MakerPosted depending on conviction
        assert!(
            ms.state == SniperState::Monitoring || ms.state == SniperState::MakerPosted,
            "State should be Monitoring or MakerPosted, got {:?}",
            ms.state
        );
    }

    #[test]
    fn test_15min_market_eligible() {
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        let pair = crate::strategy::MarketPair::new_up_down(
            "0x15m_test".to_string(),
            "up_token".to_string(),
            "down_token".to_string(),
        )
        .with_event_slug("btc-updown-15m-1740000000")
        .with_close_time(now + 20);

        registry.register(pair);

        let config = MomentumConfig::preset_5m(); // 5m defaults, but 15m detected
        let strategy = MomentumStrategy::new(registry, config);

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();
        for i in 0..900 {
            let ts = now_ms - (900_000 - i * 1000);
            history.record("btc", ts, dec!(50000) + Decimal::from(i));
        }
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50900),
            timestamp_ms: now_ms,
        });

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let _ = strategy.on_tick(&ctx);

        // Should have created a market state for the 15m market
        assert!(
            strategy.markets.contains_key("0x15m_test"),
            "15m market should be tracked"
        );
    }

    #[test]
    fn test_stop_loss_trigger() {
        let registry = Arc::new(MarketPairRegistry::new());

        let pair = crate::strategy::MarketPair::new_up_down(
            "0xsl_test".to_string(),
            "up_token_sl".to_string(),
            "down_token_sl".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000");

        registry.register(pair);

        let config = MomentumConfig::preset_5m();
        let strategy = MomentumStrategy::new(registry, config.clone());

        // Manually set up a market state in InventoryHeld
        let now_ms = chrono::Utc::now().timestamp_millis();
        strategy.markets.insert(
            "0xsl_test".to_string(),
            MarketState {
                condition_id: "0xsl_test".to_string(),
                asset: "btc".to_string(),
                timeframe: Timeframe::FiveMin,
                state: SniperState::InventoryHeld,
                maker_order_id: None,
                target_token_id: "up_token_sl".to_string(),
                entry_price: Some(dec!(0.93)),
                entry_size: Some(dec!(50)),
                maker_posted_at: Some(Instant::now()),
                last_cancel_replace: None,
                conviction: dec!(0.80),
                direction: Direction::Up,
                tp_order_id: None,
                entry_instant: Some(Instant::now()),
                entry_timestamp_ms: Some(now_ms - 5000), // Entered 5s ago
                posted_price: None,
                pending_cancel: false,
                close_time: None,
            },
        );

        // Set up spot data showing reversal
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);

        // BTC was at 50000 when we entered, now dropped to 49800 (-0.4% reversal)
        history.record("btc", now_ms - 5000, dec!(50000));
        history.record("btc", now_ms - 4000, dec!(49950));
        history.record("btc", now_ms - 3000, dec!(49900));
        history.record("btc", now_ms - 2000, dec!(49850));
        history.record("btc", now_ms - 1000, dec!(49800));
        history.record("btc", now_ms, dec!(49800));
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(49800),
            timestamp_ms: now_ms,
        });

        // Set up order book with bid
        let books = OrderBookState::new();
        books.update_book(
            "up_token_sl".to_string(),
            "0xsl_test".to_string(),
            vec![crate::api::types::PriceLevel {
                price: "0.91".to_string(),
                size: "200".to_string(),
            }],
            vec![],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        // Manually call process_inventory_held
        let mut ms = strategy.markets.get_mut("0xsl_test").unwrap();
        let intents = strategy.process_inventory_held(&mut ms, &ctx, &config);

        // Should have triggered stop-loss
        assert_eq!(intents.len(), 1, "Should have one stop-loss sell intent");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].urgency, Urgency::Normal); // FAK
    }
}
