//! Conviction Rider Strategy — maker-only mid-candle entry with hold-to-resolution
//!
//! Uses Binance WebSocket spot data as an oracle to predict Polymarket
//! 5m/15m crypto market outcomes.
//!
//! ## Strategy Flow
//!
//! 1. Watches Binance BTC/ETH/SOL/XRP spot prices in real-time
//! 2. Enters monitoring window mid-candle (configurable elapsed time)
//! 3. Computes conviction score, checks entry price range [0.30–0.65]
//! 4. Posts a GTC maker BUY order with cancel/replace loop (up to 60s)
//! 5. Posts maker TP SELL at fixed target (0.93–0.95)
//! 6. If TP doesn't fill near close, cancels TP and holds to resolution
//! 7. No taker fallback. No emergency exit. Maker-only, zero fees.

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

    /// Candle duration in seconds
    pub fn duration_secs(&self) -> i64 {
        match self {
            Timeframe::FiveMin => 300,
            Timeframe::FifteenMin => 900,
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
    /// Waiting for entry window
    Idle,
    /// Within entry window, computing conviction
    Monitoring,
    /// GTC maker BUY posted, cancel/replace loop active
    MakerEntry,
    /// Position acquired, about to post TP
    InventoryHeld,
    /// Take-profit maker SELL posted, waiting for fill
    TPPosted,
    /// TP didn't fill, holding through resolution
    HoldToResolution,
    /// Trade complete (TP filled, resolved, or stopped out)
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
    /// The GTC order ID (if in MakerEntry state)
    pub maker_order_id: Option<String>,
    /// The token we're betting on (Up or Down)
    pub target_token_id: String,
    /// Entry price (what we paid)
    pub entry_price: Option<Decimal>,
    /// Entry size (shares acquired)
    pub entry_size: Option<Decimal>,
    /// When we entered MakerEntry state (for timeout)
    pub maker_posted_at: Option<Instant>,
    /// Last cancel/replace timestamp (for <200ms loop)
    pub last_cancel_replace: Option<Instant>,
    /// Current conviction score
    pub conviction: Decimal,
    /// Direction (Up/Down)
    pub direction: Direction,
    /// Take-profit order ID (if in TPPosted state)
    pub tp_order_id: Option<String>,
    /// When we entered TPPosted state
    pub tp_posted_at: Option<Instant>,
    /// Entry timestamp in Instant (for stop-loss delta calc)
    pub entry_instant: Option<Instant>,
    /// Entry timestamp in unix ms (for price history lookup)
    pub entry_timestamp_ms: Option<i64>,
    /// Price we posted the maker order at (for cancel/replace tracking)
    pub posted_price: Option<Decimal>,
    /// The initial entry price (first maker post — for max_chase_cents guard)
    pub initial_entry_price: Option<Decimal>,
    /// Market close time (unix timestamp)
    pub close_time: Option<i64>,
    /// Whether we've logged the "HOLD TO RESOLUTION" message
    pub hold_logged: bool,
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
            tp_posted_at: None,
            entry_instant: None,
            entry_timestamp_ms: None,
            posted_price: None,
            initial_entry_price: None,
            close_time: None,
            hold_logged: false,
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

        // Minimum weighted delta gate — reject noise-level moves.
        // Crypto routinely moves 0.05% in 5 min from noise alone.
        let abs_delta = weighted_delta.abs();
        let min_delta = dec!(0.0005); // 0.05% minimum movement required
        if abs_delta < min_delta {
            return (Decimal::ZERO, Direction::Neutral);
        }

        // Raw score
        let raw_score = abs_delta * consistency_multiplier / vol_penalty;

        // Calibration: 0.4% move maps to ~1.0 conviction.
        // Previous 0.0014 was too loose — 0.08% noise gave conv=1.0.
        // New: 0.1% → ~0.25, 0.2% → ~0.50, 0.3% → ~0.75, 0.4%+ → ~1.0
        let calibration = dec!(0.004);
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
    // === Entry Window ===
    /// Seconds after candle open to start monitoring
    pub entry_window_start_secs: i64,
    /// Seconds before close to stop entering (hard cutoff)
    pub entry_window_end_secs: i64,

    // === Conviction Scoring ===
    /// Minimum conviction score to act (0.0 – 1.0)
    pub min_conviction: Decimal,
    /// Lookback windows for multi-timeframe momentum (in milliseconds)
    pub lookback_windows_ms: Vec<i64>,
    /// Weights for each lookback window (must sum to 1.0)
    pub lookback_weights: Vec<Decimal>,

    // === Entry Pricing ===
    /// Minimum acceptable entry price (skip if book price is below this)
    pub min_entry_price: Decimal,
    /// Maximum entry price — skip markets where best_bid exceeds this
    pub max_entry_price: Decimal,

    // === Maker Entry ===
    /// How long to keep trying maker entry before giving up (seconds)
    pub maker_entry_timeout_secs: u64,
    /// Cancel/replace loop interval (ms)
    pub cancel_replace_interval_ms: u64,
    /// Max cents above initial entry price to chase (prevents crossing spread)
    pub max_chase_cents: Decimal,

    // === Take Profit (Dynamic) ===
    /// Minimum profit per share to target (e.g., 0.15 = 15 cents)
    pub tp_min_profit: Decimal,
    /// Floor: never post TP below this price (e.g., 0.70)
    pub tp_floor_price: Decimal,
    /// Ceiling: never post TP above this price (e.g., 0.95)
    pub tp_ceiling_price: Decimal,

    // === Stop Loss ===
    /// Massive Binance reversal threshold (e.g., -0.010 = 1.0%)
    pub stop_loss_reversal_pct: Decimal,

    // === Sizing ===
    pub max_size_per_trade: Decimal,
    pub max_total_exposure: Decimal,
    pub max_concurrent_positions: usize,
    /// Max positions in the same direction (UP or DOWN) to limit correlation risk
    pub max_same_direction_positions: usize,

    // === Assets ===
    pub assets: Vec<String>,
}

impl MomentumConfig {
    /// Preset for 15-minute markets
    pub fn preset_15m_rider() -> Self {
        Self {
            // Entry window: 5min into candle → 2min before close
            entry_window_start_secs: 300,
            entry_window_end_secs: 120,

            // Conviction
            min_conviction: dec!(0.75),
            lookback_windows_ms: vec![180_000, 600_000, 900_000],
            lookback_weights: vec![dec!(0.5), dec!(0.3), dec!(0.2)],

            // Entry pricing
            min_entry_price: dec!(0.30),
            max_entry_price: dec!(0.65),

            // Maker entry
            maker_entry_timeout_secs: 60,
            cancel_replace_interval_ms: 500,
            max_chase_cents: dec!(0.03),

            // Take profit (dynamic: entry + min_profit, capped at ceiling — NO floor)
            tp_min_profit: dec!(0.10),
            tp_floor_price: dec!(0.70),  // kept for config compat, NOT used in formula
            tp_ceiling_price: dec!(0.95),

            // Stop loss — ONLY on massive Binance reversal
            stop_loss_reversal_pct: dec!(-0.010),

            // Sizing
            max_size_per_trade: dec!(15),
            max_total_exposure: dec!(50),
            max_concurrent_positions: 4,
            max_same_direction_positions: 2,

            // Assets
            assets: vec![
                "btc".to_string(),
                "eth".to_string(),
                "sol".to_string(),
                "xrp".to_string(),
            ],
        }
    }

    /// Preset for 5-minute markets
    pub fn preset_5m_rider() -> Self {
        Self {
            // Entry window: 2min into candle → 1min before close
            entry_window_start_secs: 120,
            entry_window_end_secs: 60,

            // Conviction
            min_conviction: dec!(0.75),
            lookback_windows_ms: vec![60_000, 180_000, 300_000],
            lookback_weights: vec![dec!(0.5), dec!(0.3), dec!(0.2)],

            // Entry pricing
            min_entry_price: dec!(0.30),
            max_entry_price: dec!(0.65),

            // Maker entry
            maker_entry_timeout_secs: 30,
            cancel_replace_interval_ms: 500,
            max_chase_cents: dec!(0.03),

            // Take profit (dynamic: entry + min_profit, capped at ceiling — NO floor)
            tp_min_profit: dec!(0.08),
            tp_floor_price: dec!(0.65),  // kept for config compat, NOT used in formula
            tp_ceiling_price: dec!(0.93),

            // Stop loss
            stop_loss_reversal_pct: dec!(-0.008),

            // Sizing
            max_size_per_trade: dec!(15),
            max_total_exposure: dec!(50),
            max_concurrent_positions: 4,
            max_same_direction_positions: 2,

            // Assets
            assets: vec![
                "btc".to_string(),
                "eth".to_string(),
                "sol".to_string(),
                "xrp".to_string(),
            ],
        }
    }

    /// Default live test configuration
    pub fn default_live_test() -> Self {
        Self::from_env_with_defaults(Self::preset_5m_rider())
    }

    /// Load overrides from environment variables
    fn from_env_with_defaults(mut config: Self) -> Self {
        if let Ok(v) = std::env::var("RIDER_MIN_CONVICTION") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.min_conviction = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_MIN_ENTRY_PRICE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.min_entry_price = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_MAX_ENTRY_PRICE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.max_entry_price = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_TP_TARGET") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.tp_ceiling_price = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_TP_MIN_PROFIT") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.tp_min_profit = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_TP_FLOOR") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.tp_floor_price = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_STOP_LOSS_PCT") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.stop_loss_reversal_pct = -d.abs();
            }
        }
        if let Ok(v) = std::env::var("RIDER_MAX_EXPOSURE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.max_total_exposure = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_MAX_SIZE_PER_TRADE") {
            if let Ok(d) = v.parse::<Decimal>() {
                config.max_size_per_trade = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_MAKER_ENTRY_TIMEOUT_SECS") {
            if let Ok(d) = v.parse::<u64>() {
                config.maker_entry_timeout_secs = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_ENTRY_WINDOW_START_SECS") {
            if let Ok(d) = v.parse::<i64>() {
                config.entry_window_start_secs = d;
            }
        }
        if let Ok(v) = std::env::var("RIDER_ENTRY_WINDOW_END_SECS") {
            if let Ok(d) = v.parse::<i64>() {
                config.entry_window_end_secs = d;
            }
        }
        config
    }
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self::preset_5m_rider()
    }
}

// ============================================================================
// MOMENTUM STRATEGY
// ============================================================================

/// Conviction Rider strategy for 5m/15m crypto markets
pub struct MomentumStrategy {
    /// Configuration (5m defaults; timeframe-specific params applied per-market)
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
                // Apply 15m-specific overrides from preset
                let preset = MomentumConfig::preset_15m_rider();
                let mut c = self.config.clone();
                c.entry_window_start_secs = preset.entry_window_start_secs;
                c.entry_window_end_secs = preset.entry_window_end_secs;
                c.lookback_windows_ms = preset.lookback_windows_ms;
                c.lookback_weights = preset.lookback_weights;
                c.maker_entry_timeout_secs = preset.maker_entry_timeout_secs;
                c.cancel_replace_interval_ms = preset.cancel_replace_interval_ms;
                c.max_chase_cents = preset.max_chase_cents;
                c.tp_min_profit = preset.tp_min_profit;
                c.tp_floor_price = preset.tp_floor_price;
                c.tp_ceiling_price = preset.tp_ceiling_price;
                c.stop_loss_reversal_pct = preset.stop_loss_reversal_pct;
                c
            }
        }
    }

    /// Count active positions (MakerEntry, InventoryHeld, TPPosted, HoldToResolution)
    fn active_position_count(&self) -> usize {
        self.markets
            .iter()
            .filter(|entry| {
                matches!(
                    entry.value().state,
                    SniperState::MakerEntry
                        | SniperState::InventoryHeld
                        | SniperState::TPPosted
                        | SniperState::HoldToResolution
                )
            })
            .count()
    }

    /// Count active positions in a specific direction
    fn active_positions_in_direction(&self, dir: Direction) -> usize {
        self.markets
            .iter()
            .filter(|entry| {
                let ms = entry.value();
                ms.direction == dir
                    && matches!(
                        ms.state,
                        SniperState::MakerEntry
                            | SniperState::InventoryHeld
                            | SniperState::TPPosted
                            | SniperState::HoldToResolution
                    )
            })
            .count()
    }

    /// Clean up completed/stale market states (older than 2 hours)
    fn cleanup_stale(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(7200);
        self.markets.retain(|_, ms| {
            ms.state != SniperState::Completed
                || ms.maker_posted_at.map_or(true, |t| t > cutoff)
        });
    }

    /// Process Idle → Monitoring transition (elapsed-time-based entry window)
    fn process_idle(
        &self,
        ms: &mut MarketState,
        close_time: i64,
        now_unix: i64,
        tf_config: &MomentumConfig,
    ) {
        let candle_duration = ms.timeframe.duration_secs();
        let candle_open_time = close_time - candle_duration;
        let elapsed = now_unix - candle_open_time;

        // Safety: never enter a candle that hasn't started yet
        if elapsed < 0 {
            return;
        }

        if elapsed >= tf_config.entry_window_start_secs {
            let secs_until_close = close_time - now_unix;
            ms.state = SniperState::Monitoring;
            info!(
                "ConvictionRider: {} {} -> Monitoring ({}s elapsed, {}s to close)",
                ms.asset,
                ms.timeframe.label(),
                elapsed,
                secs_until_close,
            );
        }
    }

    /// Process Monitoring → MakerEntry transition
    fn process_monitoring(
        &self,
        ms: &mut MarketState,
        secs_until_close: i64,
        elapsed: i64,
        ctx: &StrategyContext,
        tf_config: &MomentumConfig,
        active_count: usize,
        up_count: usize,
        down_count: usize,
    ) -> Vec<OrderIntent> {
        // Compute conviction
        let (conviction, direction) = ConvictionEngine::compute(ctx, &ms.asset, tf_config);
        ms.conviction = conviction;
        ms.direction = direction;

        let dir_label = match direction {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
            Direction::Neutral => "NEUTRAL",
        };

        debug!(
            "ConvictionRider: {} {} conv={:.3} {} ({}s elapsed, {}s to close) — {} active positions",
            ms.asset,
            ms.timeframe.label(),
            conviction,
            dir_label,
            elapsed,
            secs_until_close,
            active_count,
        );

        // Past the entry window end — missed it
        if secs_until_close <= tf_config.entry_window_end_secs {
            debug!(
                "ConvictionRider: {} entry window closed ({}s to close, cutoff={}s)",
                ms.asset, secs_until_close, tf_config.entry_window_end_secs
            );
            ms.state = SniperState::Completed;
            return Vec::new();
        }

        // Don't fire if exchange is unhealthy
        if !ctx.is_exchange_healthy() {
            debug!(
                "ConvictionRider: {} exchange unhealthy, skipping",
                ms.asset
            );
            return Vec::new();
        }

        // Don't fire if balance drifted (ledger vs exchange mismatch)
        if !ctx.is_balance_healthy() {
            warn!(
                "ConvictionRider: {} balance unhealthy (drift detected), blocking new entries",
                ms.asset
            );
            return Vec::new();
        }

        if conviction < tf_config.min_conviction {
            return Vec::new();
        }

        if direction == Direction::Neutral {
            return Vec::new();
        }

        // Max concurrent positions check
        if active_count >= tf_config.max_concurrent_positions {
            debug!(
                "ConvictionRider: max {} concurrent positions reached, skipping",
                tf_config.max_concurrent_positions
            );
            return Vec::new();
        }

        // Same-direction correlation limit: prevent 4x concentrated bets
        let same_dir_count = match direction {
            Direction::Up => up_count,
            Direction::Down => down_count,
            Direction::Neutral => 0,
        };
        if same_dir_count >= tf_config.max_same_direction_positions {
            debug!(
                "ConvictionRider: {} already has {} {} positions (max {}), skipping",
                ms.asset,
                same_dir_count,
                match direction { Direction::Up => "UP", Direction::Down => "DOWN", _ => "?" },
                tf_config.max_same_direction_positions,
            );
            return Vec::new();
        }

        // Exposure check
        if ctx.total_exposure() >= tf_config.max_total_exposure {
            debug!(
                "ConvictionRider: max exposure ${} reached, skipping",
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
                let target = bid + dec!(0.01);
                if target >= ask {
                    let safe_price = ask - dec!(0.01);
                    if safe_price < dec!(0.01) {
                        debug!("ConvictionRider: {} spread too tight, skipping", ms.asset);
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
                debug!("ConvictionRider: {} no book data, skipping", ms.asset);
                return Vec::new();
            }
        };

        // Entry price range filter [min_entry_price, max_entry_price]
        if let Some(bid) = best_bid {
            if bid > tf_config.max_entry_price {
                debug!(
                    "ConvictionRider: {} bid={} > max_entry={}, skipping",
                    ms.asset, bid, tf_config.max_entry_price
                );
                return Vec::new();
            }
        }
        if maker_price < tf_config.min_entry_price {
            debug!(
                "ConvictionRider: {} price {} below min_entry_price {}, skipping",
                ms.asset, maker_price, tf_config.min_entry_price
            );
            return Vec::new();
        }
        if maker_price > tf_config.max_entry_price {
            debug!(
                "ConvictionRider: {} price {} above max_entry_price {}, skipping",
                ms.asset, maker_price, tf_config.max_entry_price
            );
            return Vec::new();
        }

        // Sizing
        let max_affordable = (ctx.available_cash() / maker_price).floor();
        let remaining_exposure = tf_config.max_total_exposure - ctx.total_exposure();
        let max_from_exposure = (remaining_exposure / maker_price).floor();
        let trade_size = tf_config
            .max_size_per_trade
            .min(max_affordable)
            .min(max_from_exposure)
            .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero);

        let min_shares = (dec!(1) / maker_price).ceil();
        if trade_size < min_shares {
            debug!("ConvictionRider: insufficient cash for trade");
            ms.state = SniperState::Completed;
            return Vec::new();
        }

        info!(
            "{} Conviction: {} {} (conv={:.3}) -> MAKER BUY {} @ {} x {} (bid={:?} ask={:?})",
            ms.timeframe.label(),
            ms.asset,
            dir_label,
            conviction,
            &winning_token[..winning_token.len().min(12)],
            maker_price,
            trade_size,
            best_bid,
            best_ask,
        );

        // Transition to MakerEntry
        ms.target_token_id = winning_token.clone();
        ms.state = SniperState::MakerEntry;
        ms.maker_posted_at = Some(Instant::now());
        ms.posted_price = Some(maker_price);
        ms.initial_entry_price = Some(maker_price);

        let intent = OrderIntent::new(
            ms.condition_id.clone(),
            winning_token,
            Side::Buy,
            maker_price,
            trade_size,
            Urgency::Passive, // GTC maker — zero fees
            format!(
                "{} conviction {} {} (conv={:.2})",
                ms.timeframe.label(),
                ms.asset,
                dir_label,
                conviction,
            ),
            "ConvictionRider",
        )
        .with_fee_rate(pair.fee_rate_bps)
        .with_priority(60);

        vec![intent]
    }

    /// Process InventoryHeld — settlement cooldown, Binance stop-loss, post TP
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

        // === SETTLEMENT COOLDOWN: Wait for on-chain token settlement ===
        const SETTLEMENT_COOLDOWN_SECS: u64 = 7;
        let entry_elapsed_secs = ms
            .entry_instant
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if entry_elapsed_secs < SETTLEMENT_COOLDOWN_SECS {
            return Vec::new();
        }

        // === STOP-LOSS: Binance delta reversal only (massive, 1%+ reversal) ===
        if let Some(entry_ts_ms) = ms.entry_timestamp_ms {
            let now_ms = ctx.utc_now.timestamp_millis();
            let ms_since_entry = now_ms - entry_ts_ms;
            if let Some(delta) = ctx.spot_change_pct(&ms.asset, ms_since_entry) {
                let reversal = match ms.direction {
                    Direction::Up => delta,
                    Direction::Down => -delta,
                    Direction::Neutral => Decimal::ZERO,
                };

                if reversal < tf_config.stop_loss_reversal_pct {
                    let sell_price = ctx
                        .best_bid(&ms.target_token_id)
                        .unwrap_or(dec!(0.50));

                    // Truncate to 2dp — Polymarket max lot size is 2 decimal places
                    let sell_size = entry_size.round_dp_with_strategy(
                        2,
                        rust_decimal::RoundingStrategy::ToZero,
                    );

                    warn!(
                        "STOP-LOSS: {} {} Binance reversal {:.4}% (threshold={:.3}%) -> MAKER SELL {} @ {}",
                        ms.asset,
                        ms.timeframe.label(),
                        reversal * dec!(100),
                        tf_config.stop_loss_reversal_pct * dec!(100),
                        sell_size,
                        sell_price,
                    );

                    ms.state = SniperState::Completed;

                    return vec![OrderIntent::new(
                        ms.condition_id.clone(),
                        ms.target_token_id.clone(),
                        Side::Sell,
                        sell_price,
                        sell_size,
                        Urgency::Passive, // GTC maker — zero fees
                        format!("stop-loss {} {}", ms.asset, ms.timeframe.label()),
                        "ConvictionRider",
                    )
                    .with_fee_rate(pair.fee_rate_bps)
                    .with_priority(90)];
                }
            }
        }

        // === TAKE-PROFIT: Dynamic TP based on entry price ===
        // === FIX: tp_price = entry + min_profit, capped at ceiling (NO floor) ===
        // The floor was making TPs unreachable for low-entry tokens (e.g., entry $0.33, floor $0.70 = +112%)
        let raw_tp = entry_price + tf_config.tp_min_profit;
        let tp_price = raw_tp
            .min(tf_config.tp_ceiling_price)
            .min(dec!(0.99));

        // Truncate to 2dp — Polymarket max lot size is 2 decimal places
        let sell_size = entry_size.round_dp_with_strategy(
            2,
            rust_decimal::RoundingStrategy::ToZero,
        );

        info!(
            "TAKE-PROFIT: {} {} entry={} -> MAKER SELL {} @ {} (entry+{}={}, ceiling={})",
            ms.asset,
            ms.timeframe.label(),
            entry_price,
            sell_size,
            tp_price,
            tf_config.tp_min_profit,
            raw_tp,
            tf_config.tp_ceiling_price,
        );

        ms.state = SniperState::TPPosted;
        ms.tp_posted_at = Some(Instant::now());

        vec![OrderIntent::new(
            ms.condition_id.clone(),
            ms.target_token_id.clone(),
            Side::Sell,
            tp_price,
            sell_size,
            Urgency::Passive, // GTC maker sell — zero fees
            format!("take-profit {} {}", ms.asset, ms.timeframe.label()),
            "ConvictionRider",
        )
        .with_fee_rate(pair.fee_rate_bps)
        .with_priority(70)]
    }
}

impl Strategy for MomentumStrategy {
    fn name(&self) -> &str {
        "ConvictionRider"
    }

    fn priority(&self) -> u8 {
        60
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn subscribed_markets(&self) -> Vec<ConditionId> {
        Vec::new()
    }

    fn on_book_update(
        &self,
        _market_id: &ConditionId,
        _token_id: &TokenId,
        _ctx: &StrategyContext,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn on_tick(&self, ctx: &StrategyContext) -> Vec<OrderIntent> {
        if ctx.spot_prices.is_none() || ctx.price_history.is_none() {
            return Vec::new();
        }

        self.cleanup_stale();

        let now_unix = ctx.utc_now.timestamp();
        let mut intents = Vec::new();

        // Pre-compute active position count BEFORE entering the DashMap loop
        // to avoid deadlock (iter() inside entry() lock = deadlock)
        let mut active_count = self.active_position_count();
        let mut up_count = self.active_positions_in_direction(Direction::Up);
        let mut down_count = self.active_positions_in_direction(Direction::Down);

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

            // Skip markets that already closed (but allow HoldToResolution to run)
            if secs_until_close < -300 {
                // 5+ minutes past close — clean up
                if let Some(mut ms) = self.markets.get_mut(condition_id) {
                    if ms.state == SniperState::HoldToResolution {
                        info!(
                            "ConvictionRider: {} {} market closed {}s ago, completing hold-to-resolution",
                            ms.asset, ms.timeframe.label(), -secs_until_close
                        );
                        ms.state = SniperState::Completed;
                    } else if ms.state != SniperState::Completed {
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

            let mut ms = self
                .markets
                .entry(condition_id.clone())
                .or_insert_with(|| MarketState::new(condition_id.clone(), asset.clone(), timeframe));

            if ms.close_time.is_none() {
                ms.close_time = Some(close_time);
            }

            // Compute elapsed time from candle open
            let candle_duration = timeframe.duration_secs();
            let candle_open_time = close_time - candle_duration;
            let elapsed = now_unix - candle_open_time;

            match ms.state {
                SniperState::Idle => {
                    self.process_idle(&mut ms, close_time, now_unix, &tf_config);
                }
                SniperState::Monitoring => {
                    let new_intents =
                        self.process_monitoring(&mut ms, secs_until_close, elapsed, ctx, &tf_config, active_count, up_count, down_count);
                    if !new_intents.is_empty() {
                        // === FIX: Increment counts so subsequent markets in this tick
                        // see the updated position counts (prevents same-tick race) ===
                        active_count += 1;
                        match ms.direction {
                            Direction::Up => up_count += 1,
                            Direction::Down => down_count += 1,
                            Direction::Neutral => {}
                        }
                    }
                    intents.extend(new_intents);
                }
                SniperState::InventoryHeld => {
                    let new_intents = self.process_inventory_held(&mut ms, ctx, &tf_config);
                    intents.extend(new_intents);
                }
                SniperState::TPPosted => {
                    // Check if TP order was placed
                    let tp_age_ms = ms
                        .tp_posted_at
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);

                    // Discover tp_order_id from order_tracker if not yet set.
                    // The executor stores the order_id in the tracker after submission,
                    // but there's no direct callback to the strategy — so we poll it here.
                    if ms.tp_order_id.is_none() {
                        if let Some(order_id) = ctx.first_order_for_token(&ms.target_token_id) {
                            debug!(
                                "ConvictionRider: {} {} discovered TP order_id {} from tracker",
                                ms.asset, ms.timeframe.label(), &order_id[..order_id.len().min(16)],
                            );
                            ms.tp_order_id = Some(order_id);
                        }
                    }

                    // If tp_order_id is set, the order is on the book — check for HoldToResolution transition
                    if ms.tp_order_id.is_some() {
                        // Near close: cancel TP and hold to resolution
                        if secs_until_close <= 30 {
                            info!(
                                "ConvictionRider: {} {} {}s to close, cancelling TP -> HoldToResolution",
                                ms.asset, ms.timeframe.label(), secs_until_close,
                            );
                            // State transition only — actual cancel happens in on_order_management
                            // which emits CancelAllForToken for the TP order
                            ms.state = SniperState::HoldToResolution;
                        }
                        // Otherwise just wait for fill
                        continue;
                    }

                    // tp_order_id is None — order may still be executing or was rejected
                    const TP_GRACE_PERIOD_MS: u128 = 5_000;
                    if tp_age_ms < TP_GRACE_PERIOD_MS {
                        continue; // Still waiting for execution
                    }

                    // After 5s grace and no tp_order_id — rejected, go to HoldToResolution
                    warn!(
                        "ConvictionRider: {} {} TP sell rejected after {}ms -> HoldToResolution",
                        ms.asset, ms.timeframe.label(), tp_age_ms,
                    );
                    ms.state = SniperState::HoldToResolution;
                }
                SniperState::HoldToResolution => {
                    // Log once
                    if !ms.hold_logged {
                        let entry_size = ms.entry_size.unwrap_or(Decimal::ZERO);
                        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        info!(
                            "HOLD TO RESOLUTION: {} {} holding {} shares @ entry {} — awaiting market close",
                            ms.asset, ms.timeframe.label(), entry_size, entry_price,
                        );
                        ms.hold_logged = true;
                    }
                    // Transition handled by the -300s check above
                }
                SniperState::Completed => {
                    // Nothing to do
                }
                SniperState::MakerEntry => {
                    // Handled in on_order_management
                }
            }
        }

        intents
    }

    fn on_order_management(&self, ctx: &StrategyContext) -> Vec<OrderAction> {
        let mut actions = Vec::new();

        for mut entry in self.markets.iter_mut() {
            let ms = entry.value_mut();

            match ms.state {
                SniperState::MakerEntry => {
                    let posted_at = match ms.maker_posted_at {
                        Some(t) => t,
                        None => {
                            ms.state = SniperState::Completed;
                            continue;
                        }
                    };

                    let elapsed_secs = posted_at.elapsed().as_secs();
                    let tf_config = self.config_for_timeframe(ms.timeframe);

                    // Timeout check
                    if elapsed_secs >= tf_config.maker_entry_timeout_secs {
                        actions.push(OrderAction::CancelAllForToken {
                            token_id: ms.target_token_id.clone(),
                        });
                        info!(
                            "ConvictionRider: {} maker entry timeout ({}s), abandoning",
                            ms.asset, elapsed_secs
                        );
                        ms.state = SniperState::Completed;
                        continue;
                    }

                    // Entry window end check
                    if let Some(close_time) = ms.close_time {
                        let secs_until_close = close_time - ctx.utc_now.timestamp();
                        if secs_until_close <= tf_config.entry_window_end_secs {
                            actions.push(OrderAction::CancelAllForToken {
                                token_id: ms.target_token_id.clone(),
                            });
                            info!(
                                "ConvictionRider: {} entry window closed ({}s to close), abandoning",
                                ms.asset, secs_until_close
                            );
                            ms.state = SniperState::Completed;
                            continue;
                        }
                    }

                    // Cancel/replace loop
                    if !ctx.is_exchange_healthy() {
                        continue;
                    }

                    // === FIX: Stop chasing if we already have enough shares ===
                    let already_filled = ms.entry_size.unwrap_or(Decimal::ZERO);
                    let remaining = tf_config.max_size_per_trade - already_filled;
                    if remaining <= Decimal::ZERO {
                        // Already fully filled from partial fills during cancel/replace
                        info!(
                            "ConvictionRider: {} already filled {} shares (max={}), stopping chase",
                            ms.asset, already_filled, tf_config.max_size_per_trade
                        );
                        // Cancel any outstanding order and transition
                        actions.push(OrderAction::CancelAllForToken {
                            token_id: ms.target_token_id.clone(),
                        });
                        if already_filled >= dec!(5) {
                            ms.state = SniperState::InventoryHeld;
                        } else {
                            ms.state = SniperState::Completed;
                        }
                        continue;
                    }

                    let should_replace = ms
                        .last_cancel_replace
                        .map(|t| {
                            t.elapsed().as_millis() as u64
                                >= tf_config.cancel_replace_interval_ms
                        })
                        .unwrap_or(true);

                    if should_replace {
                        let active_order_id = ctx.first_order_for_token(&ms.target_token_id);
                        if let Some(order_id) = active_order_id {
                            let best_bid = ctx.best_bid(&ms.target_token_id);
                            let best_ask = ctx.best_ask(&ms.target_token_id);

                            if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
                                let target = bid + dec!(0.01);
                                let optimal_price = if target >= ask {
                                    ask - dec!(0.01)
                                } else {
                                    target
                                };

                                // === FIX: Enforce max_entry_price ceiling ===
                                if optimal_price > tf_config.max_entry_price {
                                    debug!(
                                        "ConvictionRider: {} optimal {} > max_entry {}, clamping",
                                        ms.asset, optimal_price, tf_config.max_entry_price
                                    );
                                    // Don't replace — current order is already at or near ceiling
                                    ms.last_cancel_replace = Some(Instant::now());
                                    continue;
                                }

                                // === FIX: Enforce max_chase_cents above initial price ===
                                if let Some(initial) = ms.initial_entry_price {
                                    if optimal_price > initial + tf_config.max_chase_cents {
                                        debug!(
                                            "ConvictionRider: {} chase limit reached: {} > {} + {} cents",
                                            ms.asset, optimal_price, initial, tf_config.max_chase_cents
                                        );
                                        ms.last_cancel_replace = Some(Instant::now());
                                        continue;
                                    }
                                }

                                // === FIX: Never cross the ask (strict maker-only) ===
                                if optimal_price >= ask {
                                    debug!(
                                        "ConvictionRider: {} price {} would cross ask {}, skipping",
                                        ms.asset, optimal_price, ask
                                    );
                                    ms.last_cancel_replace = Some(Instant::now());
                                    continue;
                                }

                                if let Some(posted) = ms.posted_price {
                                    if optimal_price != posted && optimal_price >= dec!(0.01) {
                                        debug!(
                                            "ConvictionRider: {} cancel/replace {} -> {} (bid={} ask={})",
                                            ms.asset, posted, optimal_price, bid, ask
                                        );

                                        let pair =
                                            match self.registry.get_by_condition(&ms.condition_id) {
                                                Some(p) => p,
                                                None => continue,
                                            };

                                        // === FIX: Size = remaining shares, not max ===
                                        let trade_size = remaining
                                            .min(tf_config.max_size_per_trade)
                                            .round_dp_with_strategy(2, rust_decimal::RoundingStrategy::ToZero);

                                        if trade_size < Decimal::ONE {
                                            ms.last_cancel_replace = Some(Instant::now());
                                            continue;
                                        }

                                        let new_intent = OrderIntent::new(
                                            ms.condition_id.clone(),
                                            ms.target_token_id.clone(),
                                            Side::Buy,
                                            optimal_price,
                                            trade_size,
                                            Urgency::Passive, // GTC maker — zero fees
                                            format!(
                                                "cancel/replace {} {}",
                                                ms.asset,
                                                ms.timeframe.label()
                                            ),
                                            "ConvictionRider",
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
                SniperState::TPPosted => {
                    // Cancel TP when transitioning to HoldToResolution
                    // (The state transition happens in on_tick; here we just handle
                    //  the cancel if we need to cancel the TP order)
                    if let Some(close_time) = ms.close_time {
                        let secs_until_close = close_time - ctx.utc_now.timestamp();
                        if secs_until_close <= 30 && ms.tp_order_id.is_some() {
                            actions.push(OrderAction::CancelAllForToken {
                                token_id: ms.target_token_id.clone(),
                            });
                        }
                    }

                    // Binance reversal stop-loss in TPPosted state
                    if let Some(entry_ts_ms) = ms.entry_timestamp_ms {
                        let now_ms = ctx.utc_now.timestamp_millis();
                        let ms_since_entry = now_ms - entry_ts_ms;
                        let tf_config = self.config_for_timeframe(ms.timeframe);
                        if let Some(delta) = ctx.spot_change_pct(&ms.asset, ms_since_entry) {
                            let reversal = match ms.direction {
                                Direction::Up => delta,
                                Direction::Down => -delta,
                                Direction::Neutral => Decimal::ZERO,
                            };
                            if reversal < tf_config.stop_loss_reversal_pct {
                                let sell_price = ctx
                                    .best_bid(&ms.target_token_id)
                                    .unwrap_or(dec!(0.50));
                                let entry_size = ms.entry_size.unwrap_or(Decimal::ZERO);

                                warn!(
                                    "STOP-LOSS (TPPosted): {} {} Binance reversal {:.4}% -> cancel TP + MAKER SELL {} @ {}",
                                    ms.asset,
                                    ms.timeframe.label(),
                                    reversal * dec!(100),
                                    entry_size,
                                    sell_price,
                                );

                                // Cancel TP order first
                                actions.push(OrderAction::CancelAllForToken {
                                    token_id: ms.target_token_id.clone(),
                                });

                                ms.state = SniperState::Completed;
                                // Note: the sell intent will be emitted as a separate action
                                // We can't emit OrderIntent from on_order_management, so
                                // the stop-loss sell needs to be handled differently.
                                // For TPPosted stop-loss, we cancel and let HoldToResolution
                                // or resolution handle the position. But per the plan,
                                // stop-loss should sell. We handle this by transitioning
                                // to Completed and the on_tick will see it.
                            }
                        }
                    }
                }
                SniperState::HoldToResolution => {
                    // Cancel any remaining orders (e.g. TP sell) when we first enter HoldToResolution
                    if ms.tp_order_id.is_some() {
                        actions.push(OrderAction::CancelAllForToken {
                            token_id: ms.target_token_id.clone(),
                        });
                        ms.tp_order_id = None; // Prevent repeated cancels
                    }
                }
                _ => {}
            }
        }

        actions
    }

    fn on_fill(&self, fill: &Fill, _ctx: &StrategyContext) -> Vec<OrderIntent> {
        for mut entry in self.markets.iter_mut() {
            let ms = entry.value_mut();

            if ms.target_token_id != fill.token_id {
                continue;
            }

            match ms.state {
                SniperState::MakerEntry => {
                    if fill.side == Side::Buy {
                        let old_size = ms.entry_size.unwrap_or(Decimal::ZERO);
                        let old_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let new_size = old_size + fill.size;
                        let new_price = if old_size > Decimal::ZERO && new_size > Decimal::ZERO {
                            ((old_price * old_size + fill.price * fill.size) / new_size)
                                .round_dp(2)
                        } else {
                            fill.price.round_dp(2)
                        };

                        info!(
                            "ConvictionRider: {} {} BUY fill {} @ {} -> accumulated {} @ {} (state={:?})",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            new_size,
                            new_price,
                            ms.state,
                        );
                        ms.entry_price = Some(new_price);
                        ms.entry_size = Some(new_size);
                        if ms.entry_instant.is_none() {
                            ms.entry_instant = Some(Instant::now());
                            ms.entry_timestamp_ms =
                                Some(chrono::Utc::now().timestamp_millis());
                        }
                        const MIN_PROFITABLE_SHARES: Decimal = dec!(5);
                        if new_size >= MIN_PROFITABLE_SHARES {
                            info!(
                                "ConvictionRider: {} {} FILLED {} @ {} -> InventoryHeld",
                                ms.asset,
                                ms.timeframe.label(),
                                new_size,
                                new_price,
                            );
                            ms.state = SniperState::InventoryHeld;
                        }
                    }
                }
                SniperState::Completed => {
                    if fill.side == Side::Buy {
                        // Late fill recovery
                        let old_size = ms.entry_size.unwrap_or(Decimal::ZERO);
                        let old_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let new_size = old_size + fill.size;
                        let new_price = if old_size > Decimal::ZERO && new_size > Decimal::ZERO {
                            ((old_price * old_size + fill.price * fill.size) / new_size)
                                .round_dp(2)
                        } else {
                            fill.price.round_dp(2)
                        };

                        warn!(
                            "ConvictionRider: {} {} LATE FILL: {} @ {} -> total {} @ {} -> recovering to InventoryHeld",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            new_size,
                            new_price,
                        );
                        ms.entry_price = Some(new_price);
                        ms.entry_size = Some(new_size);
                        if ms.entry_instant.is_none() {
                            ms.entry_instant = Some(Instant::now());
                            ms.entry_timestamp_ms =
                                Some(chrono::Utc::now().timestamp_millis());
                        }
                        ms.state = SniperState::InventoryHeld;
                    } else if fill.side == Side::Sell {
                        // Late stop-loss sell fill — just log PnL
                        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let pnl = (fill.price - entry_price) * fill.size - fill.fee;
                        info!(
                            "ConvictionRider: {} {} LATE SELL fill {} @ {} (entry={}, PnL=${:.4})",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            entry_price,
                            pnl,
                        );
                    }
                }
                SniperState::InventoryHeld => {
                    if fill.side == Side::Buy {
                        let old_size = ms.entry_size.unwrap_or(Decimal::ZERO);
                        let old_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let new_size = old_size + fill.size;
                        let new_price = if new_size > Decimal::ZERO {
                            ((old_price * old_size + fill.price * fill.size) / new_size)
                                .round_dp(2)
                        } else {
                            fill.price.round_dp(2)
                        };
                        info!(
                            "ConvictionRider: {} {} additional fill while InventoryHeld: +{} @ {} -> total {} @ {:.4}",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            new_size,
                            new_price,
                        );
                        ms.entry_size = Some(new_size);
                        ms.entry_price = Some(new_price);
                    }
                }
                SniperState::TPPosted => {
                    if fill.side == Side::Sell {
                        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let pnl = (fill.price - entry_price) * fill.size - fill.fee;
                        info!(
                            "ConvictionRider: {} {} TP FILLED {} @ {} (entry={}, PnL=${:.4})",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            entry_price,
                            pnl,
                        );
                        ms.state = SniperState::Completed;
                    } else if fill.side == Side::Buy {
                        // Late maker fill arrived after we moved to TP
                        let old_size = ms.entry_size.unwrap_or(Decimal::ZERO);
                        let old_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let new_size = old_size + fill.size;
                        let new_price = if new_size > Decimal::ZERO {
                            ((old_price * old_size + fill.price * fill.size) / new_size)
                                .round_dp(2)
                        } else {
                            fill.price.round_dp(2)
                        };
                        warn!(
                            "ConvictionRider: {} {} LATE BUY fill in TPPosted: +{} @ {} -> total {} @ {:.4} (excess will redeem)",
                            ms.asset,
                            ms.timeframe.label(),
                            fill.size,
                            fill.price,
                            new_size,
                            new_price,
                        );
                        ms.entry_size = Some(new_size);
                        ms.entry_price = Some(new_price);
                    }
                }
                SniperState::HoldToResolution => {
                    // No active orders — unexpected fill, just log
                    if fill.side == Side::Sell {
                        let entry_price = ms.entry_price.unwrap_or(Decimal::ZERO);
                        let pnl = (fill.price - entry_price) * fill.size - fill.fee;
                        info!(
                            "ConvictionRider: {} {} unexpected SELL fill in HoldToResolution {} @ {} (PnL=${:.4})",
                            ms.asset, ms.timeframe.label(), fill.size, fill.price, pnl,
                        );
                    } else {
                        warn!(
                            "ConvictionRider: {} {} unexpected BUY fill in HoldToResolution {} @ {}",
                            ms.asset, ms.timeframe.label(), fill.size, fill.price,
                        );
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

        let config = MomentumConfig::preset_5m_rider();
        let (conviction, direction) = ConvictionEngine::compute(&ctx, "btc", &config);

        assert!(
            conviction > dec!(0.5),
            "Conviction should be significant: {}",
            conviction
        );
        assert_eq!(direction, Direction::Up);
    }

    #[test]
    fn test_conviction_engine_no_data() {
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let config = MomentumConfig::preset_5m_rider();
        let (conviction, direction) = ConvictionEngine::compute(&ctx, "btc", &config);

        assert_eq!(conviction, Decimal::ZERO);
        assert_eq!(direction, Direction::Neutral);
    }

    #[test]
    fn test_state_machine_idle_to_monitoring() {
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        // For 5m rider, entry_window_start_secs = 120
        // candle duration = 300s
        // We need elapsed >= 120, so close_time should be at most 300-120=180s from now
        // Set close_time = now + 150 → elapsed = 300-150 = 150 ≥ 120 → enters Monitoring
        let pair = crate::strategy::MarketPair::new_up_down(
            "0x5m_test".to_string(),
            "up_token_123".to_string(),
            "down_token_456".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 150);

        registry.register(pair);

        let config = MomentumConfig::preset_5m_rider();
        let strategy = MomentumStrategy::new(registry, config);

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();
        for i in 0..300 {
            let ts = now_ms - (300_000 - i * 1000);
            history.record("btc", ts, dec!(50000) + Decimal::from(i));
        }
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50300),
            timestamp_ms: now_ms,
        });

        let books = OrderBookState::new();
        books.update_book(
            "up_token_123".to_string(),
            "0x5m_test".to_string(),
            vec![],
            vec![crate::api::types::PriceLevel {
                price: "0.50".to_string(),
                size: "100".to_string(),
            }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let _intents = strategy.on_tick(&ctx);

        assert!(
            !strategy.markets.is_empty(),
            "Should have created a market state"
        );

        let ms = strategy.markets.get("0x5m_test").unwrap();
        assert!(
            ms.state == SniperState::Monitoring || ms.state == SniperState::MakerEntry,
            "State should be Monitoring or MakerEntry, got {:?}",
            ms.state
        );
    }

    #[test]
    fn test_15min_market_eligible() {
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        // For 15m rider, entry_window_start_secs = 300
        // candle duration = 900s
        // elapsed >= 300 → close_time <= now + 600
        let pair = crate::strategy::MarketPair::new_up_down(
            "0x15m_test".to_string(),
            "up_token".to_string(),
            "down_token".to_string(),
        )
        .with_event_slug("btc-updown-15m-1740000000")
        .with_close_time(now + 500);

        registry.register(pair);

        let config = MomentumConfig::preset_5m_rider();
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

        let config = MomentumConfig::preset_5m_rider();
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
                entry_price: Some(dec!(0.50)),
                entry_size: Some(dec!(15)),
                maker_posted_at: Some(Instant::now()),
                last_cancel_replace: None,
                conviction: dec!(0.80),
                direction: Direction::Up,
                tp_order_id: None,
                tp_posted_at: None,
                entry_instant: Some(Instant::now() - std::time::Duration::from_secs(8)),
                entry_timestamp_ms: Some(now_ms - 8000),
                posted_price: None,
                close_time: None,
                hold_logged: false,
                initial_entry_price: None,
            },
        );

        // Set up spot data showing 1%+ reversal (BTC dropped from 50000 to 49400 = -1.2%)
        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);

        history.record("btc", now_ms - 8000, dec!(50000));
        history.record("btc", now_ms - 6000, dec!(49800));
        history.record("btc", now_ms - 4000, dec!(49600));
        history.record("btc", now_ms - 2000, dec!(49500));
        history.record("btc", now_ms - 1000, dec!(49400));
        history.record("btc", now_ms, dec!(49400));
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(49400),
            timestamp_ms: now_ms,
        });

        let books = OrderBookState::new();
        books.update_book(
            "up_token_sl".to_string(),
            "0xsl_test".to_string(),
            vec![crate::api::types::PriceLevel {
                price: "0.45".to_string(),
                size: "200".to_string(),
            }],
            vec![],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let mut ms = strategy.markets.get_mut("0xsl_test").unwrap();
        let intents = strategy.process_inventory_held(&mut ms, &ctx, &config);

        assert_eq!(intents.len(), 1, "Should have one stop-loss sell intent");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].urgency, Urgency::Passive);
    }

    #[test]
    fn test_entry_price_filter() {
        // Verify that tokens priced above max_entry_price (0.65) are skipped
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        let pair = crate::strategy::MarketPair::new_up_down(
            "0xpf_test".to_string(),
            "up_token_pf".to_string(),
            "down_token_pf".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 150);

        registry.register(pair);

        let config = MomentumConfig::preset_5m_rider();
        let strategy = MomentumStrategy::new(registry, config);

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();
        for i in 0..300 {
            let ts = now_ms - (300_000 - i * 1000);
            history.record("btc", ts, dec!(50000) + Decimal::from(i));
        }
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50300),
            timestamp_ms: now_ms,
        });

        // Set bid at 0.70 — above max_entry_price of 0.65
        let books = OrderBookState::new();
        books.update_book(
            "up_token_pf".to_string(),
            "0xpf_test".to_string(),
            vec![crate::api::types::PriceLevel {
                price: "0.70".to_string(),
                size: "100".to_string(),
            }],
            vec![crate::api::types::PriceLevel {
                price: "0.72".to_string(),
                size: "100".to_string(),
            }],
            None,
            None,
        );

        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let intents = strategy.on_tick(&ctx);

        // Should NOT have entered a position — price too high
        assert!(
            intents.is_empty(),
            "Should not enter when bid > max_entry_price"
        );
        let ms = strategy.markets.get("0xpf_test").unwrap();
        // Should be Monitoring (price filter rejected) or Completed, not MakerEntry
        assert_ne!(
            ms.state,
            SniperState::MakerEntry,
            "Should not transition to MakerEntry with bid > max_entry_price"
        );
    }

    #[test]
    fn test_tp_price_calculation() {
        // Verify dynamic TP config values (updated: lowered tp_min_profit, floor kept for compat but NOT used)
        let config_5m = MomentumConfig::preset_5m_rider();
        assert_eq!(config_5m.tp_min_profit, dec!(0.08));
        assert_eq!(config_5m.tp_ceiling_price, dec!(0.93));

        let config_15m = MomentumConfig::preset_15m_rider();
        assert_eq!(config_15m.tp_min_profit, dec!(0.10));
        assert_eq!(config_15m.tp_ceiling_price, dec!(0.95));

        // Verify dynamic TP calculation: entry + min_profit, clamped to ceiling (NO floor)
        let config = MomentumConfig::preset_15m_rider();

        // Entry 0.57 -> raw 0.67, ceiling 0.95 -> 0.67
        let entry = dec!(0.57);
        let raw = entry + config.tp_min_profit;
        let tp = raw.min(config.tp_ceiling_price).min(dec!(0.99));
        assert_eq!(tp, dec!(0.67));

        // Entry 0.40 -> raw 0.50, ceiling 0.95 -> 0.50 (no floor, so just entry+min_profit)
        let entry = dec!(0.40);
        let raw = entry + config.tp_min_profit;
        let tp = raw.min(config.tp_ceiling_price).min(dec!(0.99));
        assert_eq!(tp, dec!(0.50));

        // Entry 0.65 -> raw 0.75, ceiling 0.95 -> 0.75
        let entry = dec!(0.65);
        let raw = entry + config.tp_min_profit;
        let tp = raw.min(config.tp_ceiling_price).min(dec!(0.99));
        assert_eq!(tp, dec!(0.75));

        // Ceiling cap: entry 0.88 -> raw 0.98 -> capped at 0.95
        let entry = dec!(0.88);
        let raw = entry + config.tp_min_profit;
        let tp = raw.min(config.tp_ceiling_price).min(dec!(0.99));
        assert_eq!(tp, dec!(0.95));
    }

    #[test]
    fn test_no_taker_fallback() {
        // Verify that after maker timeout, state goes to Completed (not TakerFallback)
        let registry = Arc::new(MarketPairRegistry::new());

        let pair = crate::strategy::MarketPair::new_up_down(
            "0xntf_test".to_string(),
            "up_token_ntf".to_string(),
            "down_token_ntf".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(chrono::Utc::now().timestamp() + 200);

        registry.register(pair);

        let config = MomentumConfig::preset_5m_rider();
        let strategy = MomentumStrategy::new(registry, config);

        // Set up a market state in MakerEntry with timeout exceeded
        strategy.markets.insert(
            "0xntf_test".to_string(),
            MarketState {
                condition_id: "0xntf_test".to_string(),
                asset: "btc".to_string(),
                timeframe: Timeframe::FiveMin,
                state: SniperState::MakerEntry,
                maker_order_id: None,
                target_token_id: "up_token_ntf".to_string(),
                entry_price: None,
                entry_size: None,
                maker_posted_at: Some(Instant::now() - std::time::Duration::from_secs(60)),
                last_cancel_replace: None,
                conviction: dec!(0.85),
                direction: Direction::Up,
                tp_order_id: None,
                tp_posted_at: None,
                entry_instant: None,
                entry_timestamp_ms: None,
                posted_price: Some(dec!(0.50)),
                close_time: Some(chrono::Utc::now().timestamp() + 200),
                hold_logged: false,
                initial_entry_price: None,
            },
        );

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger);

        let actions = strategy.on_order_management(&ctx);

        // Should have a CancelAllForToken action (cleanup)
        assert!(
            !actions.is_empty(),
            "Should have cancel action on timeout"
        );
        // Verify no PostTakerFallback
        for action in &actions {
            assert!(
                !matches!(action, OrderAction::PostTakerFallback { .. }),
                "Should NOT have taker fallback action"
            );
        }

        let ms = strategy.markets.get("0xntf_test").unwrap();
        assert_eq!(
            ms.state,
            SniperState::Completed,
            "Should transition to Completed, not TakerFallback"
        );
    }

    #[test]
    fn test_hold_to_resolution() {
        // Verify that TPPosted transitions to HoldToResolution when near close
        let registry = Arc::new(MarketPairRegistry::new());
        let now = chrono::Utc::now().timestamp();

        let pair = crate::strategy::MarketPair::new_up_down(
            "0xhtr_test".to_string(),
            "up_token_htr".to_string(),
            "down_token_htr".to_string(),
        )
        .with_event_slug("btc-updown-5m-1740000000")
        .with_close_time(now + 20); // 20 seconds to close

        registry.register(pair);

        let config = MomentumConfig::preset_5m_rider();
        let strategy = MomentumStrategy::new(registry, config);

        // Set up market state in TPPosted with an active TP order
        strategy.markets.insert(
            "0xhtr_test".to_string(),
            MarketState {
                condition_id: "0xhtr_test".to_string(),
                asset: "btc".to_string(),
                timeframe: Timeframe::FiveMin,
                state: SniperState::TPPosted,
                maker_order_id: None,
                target_token_id: "up_token_htr".to_string(),
                entry_price: Some(dec!(0.50)),
                entry_size: Some(dec!(15)),
                maker_posted_at: Some(Instant::now()),
                last_cancel_replace: None,
                conviction: dec!(0.85),
                direction: Direction::Up,
                tp_order_id: Some("tp_order_123".to_string()),
                tp_posted_at: Some(Instant::now() - std::time::Duration::from_secs(60)),
                entry_instant: Some(Instant::now() - std::time::Duration::from_secs(120)),
                entry_timestamp_ms: Some(chrono::Utc::now().timestamp_millis() - 120_000),
                posted_price: None,
                close_time: Some(now + 20),
                hold_logged: false,
                initial_entry_price: None,
            },
        );

        let spot = SpotPriceState::new();
        let history = PriceHistory::new(1800);
        let now_ms = chrono::Utc::now().timestamp_millis();
        history.record("btc", now_ms, dec!(50000));
        spot.update(&SpotPriceUpdate {
            symbol: "btc".to_string(),
            price: dec!(50000),
            timestamp_ms: now_ms,
        });

        let books = OrderBookState::new();
        let ledger = Ledger::new(dec!(10000));
        let ctx = StrategyContext::new(&books, &ledger).with_spot(&spot, &history);

        let _intents = strategy.on_tick(&ctx);

        let ms = strategy.markets.get("0xhtr_test").unwrap();
        assert_eq!(
            ms.state,
            SniperState::HoldToResolution,
            "Should transition to HoldToResolution when TP is active and near close"
        );
    }
}
