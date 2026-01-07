//! Paper trader - main orchestrator for paper trading
//!
//! Coordinates all paper trading components:
//! - Processes order intents through fill simulation
//! - Tracks simulated positions
//! - Aggregates analytics
//! - Generates reports

use crate::api::types::Side;
use crate::paper::analytics::{PaperAnalytics, QuickStats};
use crate::paper::fill_simulator::{ArbSimulation, FillSimulator, FillSimulatorConfig, SimulatedFill};
use crate::paper::position_tracker::{PositionSide, PositionTracker};
use crate::paper::report::PaperReport;
use crate::state::OrderBookState;
use crate::strategy::{MarketPairRegistry, OrderIntent};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ============================================================================
// PAPER CONFIG
// ============================================================================

/// Configuration for paper trading
#[derive(Debug, Clone)]
pub struct PaperConfig {
    /// Starting simulated capital
    pub starting_capital: Decimal,

    /// Taker fee rate (default 3%)
    pub taker_fee_rate: Decimal,

    /// Maker fee rate (default 0%)
    pub maker_fee_rate: Decimal,

    /// Whether to simulate as maker (0% fees)
    pub use_maker_mode: bool,

    /// Interval for auto-generating reports
    pub report_interval: Duration,

    /// Directory to save reports
    pub report_dir: PathBuf,

    /// Enable detailed logging of each simulated fill
    pub verbose_logging: bool,

    /// Fill simulator config
    pub fill_simulator: FillSimulatorConfig,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self {
            starting_capital: dec!(1000),
            taker_fee_rate: dec!(0.03),
            maker_fee_rate: Decimal::ZERO,
            use_maker_mode: false,
            report_interval: Duration::from_secs(300), // 5 minutes
            report_dir: PathBuf::from("./paper_reports"),
            verbose_logging: false,
            fill_simulator: FillSimulatorConfig::default(),
        }
    }
}

impl PaperConfig {
    /// Create config with maker mode enabled
    pub fn maker_mode() -> Self {
        Self {
            use_maker_mode: true,
            ..Default::default()
        }
    }

    /// Set starting capital
    pub fn with_capital(mut self, capital: Decimal) -> Self {
        self.starting_capital = capital;
        self
    }

    /// Set report directory
    pub fn with_report_dir(mut self, dir: PathBuf) -> Self {
        self.report_dir = dir;
        self
    }

    /// Enable verbose logging
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose_logging = verbose;
        self
    }
}

// ============================================================================
// PAPER STATS (for heartbeat)
// ============================================================================

/// Quick stats for heartbeat logging
#[derive(Debug, Clone)]
pub struct PaperStats {
    /// Opportunities detected
    pub opportunities: u64,
    /// Total simulated fills
    pub simulated_fills: u64,
    /// Arb attempts
    pub arb_attempts: u64,
    /// Successful arbs (both legs fill)
    pub successful_arbs: u64,
    /// Partial arbs (one leg only - dangerous!)
    pub partial_arbs: u64,
    /// Simulated P&L as taker
    pub simulated_pnl_taker: Decimal,
    /// Simulated P&L as maker
    pub simulated_pnl_maker: Decimal,
    /// Open positions count
    pub open_positions: usize,
}

// ============================================================================
// PAPER TRADER
// ============================================================================

/// Main paper trading orchestrator
pub struct PaperTrader {
    /// Configuration
    config: PaperConfig,

    /// Fill simulator
    fill_simulator: FillSimulator,

    /// Position tracker
    position_tracker: PositionTracker,

    /// Analytics aggregator
    analytics: PaperAnalytics,

    /// Market pair registry (for finding complement tokens)
    market_registry: Arc<MarketPairRegistry>,

    /// Last report time
    last_report_time: Instant,

    /// Session start time
    started_at: Instant,

    /// Total fills simulated
    total_fills: u64,

    /// All arb simulations (for CSV export)
    arb_history: Vec<ArbSimulation>,
}

impl PaperTrader {
    /// Create a new paper trader
    pub fn new(config: PaperConfig, market_registry: Arc<MarketPairRegistry>) -> Self {
        let position_tracker = PositionTracker::new(config.starting_capital)
            .with_fees(config.taker_fee_rate, config.maker_fee_rate);

        let analytics = PaperAnalytics::new()
            .with_fees(config.taker_fee_rate, config.maker_fee_rate);

        let fill_simulator = FillSimulator::with_config(config.fill_simulator.clone());

        Self {
            config,
            fill_simulator,
            position_tracker,
            analytics,
            market_registry,
            last_report_time: Instant::now(),
            started_at: Instant::now(),
            total_fills: 0,
            arb_history: Vec::new(),
        }
    }

    /// Set maker mode
    pub fn set_maker_mode(&mut self, use_maker: bool) {
        self.config.use_maker_mode = use_maker;
        self.position_tracker.set_maker_mode(use_maker);
    }

    /// Process order intents from strategy
    ///
    /// This is the main entry point - called by bot instead of real executor
    pub fn process_intents(
        &mut self,
        intents: Vec<OrderIntent>,
        order_book: &OrderBookState,
    ) -> Vec<SimulatedFill> {
        if intents.is_empty() {
            return Vec::new();
        }

        self.analytics.record_opportunity();

        // Group intents by group_id for arb handling
        let mut grouped: HashMap<Option<String>, Vec<OrderIntent>> = HashMap::new();
        for intent in intents {
            grouped
                .entry(intent.group_id.clone())
                .or_default()
                .push(intent);
        }

        let mut all_fills = Vec::new();

        for (group_id, group_intents) in grouped {
            if group_id.is_some() && group_intents.len() == 2 {
                // This is an arb pair
                let fills = self.process_arb_pair(&group_intents, order_book);
                all_fills.extend(fills);
            } else {
                // Single orders or non-paired
                for intent in group_intents {
                    let fill = self.process_single_intent(&intent, order_book);
                    all_fills.push(fill);
                }
            }
        }

        // Check if we should auto-generate report
        if self.last_report_time.elapsed() >= self.config.report_interval {
            self.generate_report();
            self.last_report_time = Instant::now();
        }

        all_fills
    }

    /// Process a single order intent
    fn process_single_intent(
        &mut self,
        intent: &OrderIntent,
        order_book: &OrderBookState,
    ) -> SimulatedFill {
        // Get book for this token
        let book = order_book
            .get_book(&intent.token_id)
            .unwrap_or_else(|| crate::state::BookSnapshot {
                token_id: intent.token_id.clone(),
                market: intent.market_id.clone(),
                bids: vec![],
                asks: vec![],
                last_update: Some(0),
                hash: Some(String::new()),
            });

        // Simulate fill
        let fill = self.fill_simulator.simulate(intent, &book);

        // Log if verbose
        if self.config.verbose_logging {
            self.log_fill(&fill);
        }

        // Record in analytics
        self.analytics.record_fill(fill.clone());
        self.total_fills += 1;

        // Update position tracker if filled
        if fill.would_fill() {
            let market_id = intent.market_id.clone();
            let side = self.determine_position_side(&intent.token_id);
            self.position_tracker.record_fill(&fill, &market_id, side);
        }

        fill
    }

    /// Process an arb pair (two linked orders)
    fn process_arb_pair(
        &mut self,
        intents: &[OrderIntent],
        order_book: &OrderBookState,
    ) -> Vec<SimulatedFill> {
        assert_eq!(intents.len(), 2, "Arb pair must have exactly 2 intents");

        let intent1 = &intents[0];
        let intent2 = &intents[1];

        // Get books for both tokens
        let book1 = order_book
            .get_book(&intent1.token_id)
            .unwrap_or_else(|| crate::state::BookSnapshot {
                token_id: intent1.token_id.clone(),
                market: intent1.market_id.clone(),
                bids: vec![],
                asks: vec![],
                last_update: Some(0),
                hash: Some(String::new()),
            });

        let book2 = order_book
            .get_book(&intent2.token_id)
            .unwrap_or_else(|| crate::state::BookSnapshot {
                token_id: intent2.token_id.clone(),
                market: intent2.market_id.clone(),
                bids: vec![],
                asks: vec![],
                last_update: Some(0),
                hash: Some(String::new()),
            });

        // Simulate arb
        let arb = self.fill_simulator.simulate_arb(
            intent1,
            intent2,
            &book1,
            &book2,
            self.config.taker_fee_rate,
        );

        // Log arb result
        self.log_arb(&arb);

        // Record in analytics
        self.analytics.record_arb(arb.clone());
        self.arb_history.push(arb.clone());

        // Record individual fills
        self.analytics.record_fill(arb.leg1.clone());
        self.analytics.record_fill(arb.leg2.clone());
        self.total_fills += 2;

        // Update positions if both legs fill
        if arb.both_would_fill {
            let market_id = intent1.market_id.clone();

            let side1 = self.determine_position_side(&intent1.token_id);
            self.position_tracker.record_fill(&arb.leg1, &market_id, side1);

            let side2 = self.determine_position_side(&intent2.token_id);
            self.position_tracker.record_fill(&arb.leg2, &market_id, side2);
        } else if arb.partial_arb {
            // One leg filled but not the other - log warning
            warn!(
                "⚠️  PARTIAL ARB: Only one leg would fill! Group: {}",
                arb.group_id
            );
        }

        vec![arb.leg1, arb.leg2]
    }

    /// Record a book update (for analytics)
    pub fn record_book_update(&mut self) {
        self.analytics.record_book_update();
    }

    /// Get quick stats for heartbeat
    pub fn stats(&self) -> PaperStats {
        let quick = self.analytics.quick_stats();

        PaperStats {
            opportunities: quick.opportunities,
            simulated_fills: self.total_fills,
            arb_attempts: quick.arb_attempts,
            successful_arbs: quick.successful_arbs,
            partial_arbs: quick.partial_arbs,
            simulated_pnl_taker: quick.net_pnl_taker,
            simulated_pnl_maker: quick.net_pnl_maker,
            open_positions: self.position_tracker.open_count(),
        }
    }

    /// Generate and save a report
    pub fn generate_report(&self) -> PaperReport {
        let position_summary = Some(self.position_tracker.summary());
        let summary = self.analytics.summary(position_summary);
        let report = PaperReport::new(summary);

        // Log to console
        info!("\n{}", report.to_console());

        // Save to file
        if let Err(e) = report.save(&self.config.report_dir, "paper_trading") {
            warn!("Failed to save paper trading report: {}", e);
        }

        report
    }

    /// Get current report without saving
    pub fn current_report(&self) -> PaperReport {
        let position_summary = Some(self.position_tracker.summary());
        let summary = self.analytics.summary(position_summary);
        PaperReport::new(summary)
    }

    /// Export arb history to CSV
    pub fn export_arbs(&self, path: &std::path::Path) -> std::io::Result<()> {
        PaperReport::export_arbs_csv(&self.arb_history, path)
    }

    /// Get session duration
    pub fn session_duration(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Reset all data (start fresh)
    pub fn reset(&mut self) {
        self.position_tracker = PositionTracker::new(self.config.starting_capital)
            .with_fees(self.config.taker_fee_rate, self.config.maker_fee_rate);
        self.analytics.reset();
        self.arb_history.clear();
        self.total_fills = 0;
        self.started_at = Instant::now();
        self.last_report_time = Instant::now();
    }

    // Helper methods

    /// Determine if token is YES or NO based on market registry
    fn determine_position_side(&self, token_id: &str) -> PositionSide {
        // Try to determine if this is a YES or NO token
        if let Some(pair) = self.market_registry.get_by_token(&token_id.to_string()) {
            if token_id == pair.first_token_id() {
                PositionSide::LongYes
            } else {
                PositionSide::LongNo
            }
        } else {
            // Default to YES if we can't determine
            PositionSide::LongYes
        }
    }

    fn log_fill(&self, fill: &SimulatedFill) {
        let status = if fill.would_fill() { "✅" } else { "❌" };
        let token_short = &fill.intent.token_id[..fill.intent.token_id.len().min(12)];

        debug!(
            "{} Simulated: {} {} {} @ ${} → {:?}",
            status,
            format!("{:?}", fill.intent.side),
            fill.intent.size,
            token_short,
            fill.intent.price,
            fill.outcome
        );
    }

    fn log_arb(&self, arb: &ArbSimulation) {
        let status = if arb.both_would_fill {
            "✅"
        } else if arb.partial_arb {
            "⚠️"
        } else {
            "❌"
        };

        info!(
            "{} Arb [{}]: cost=${:.2} edge=${:.4} ({:.2}%) | Taker: ${:.4} | Maker: ${:.4}",
            status,
            &arb.group_id[..arb.group_id.len().min(8)],
            arb.combined_cost,
            arb.gross_edge,
            arb.gross_edge_percent,
            arb.net_pnl_taker,
            arb.net_pnl_maker
        );
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::PriceLevel;
    use crate::state::BookSnapshot;
    use crate::strategy::{MarketPair, Urgency};

    fn setup_trader() -> (PaperTrader, Arc<MarketPairRegistry>) {
        let registry = Arc::new(MarketPairRegistry::new());
        registry.register(MarketPair::new(
            "market-1".to_string(),
            "token-yes".to_string(),
            "token-no".to_string(),
        ));

        let config = PaperConfig::default().with_capital(dec!(1000));
        let trader = PaperTrader::new(config, registry.clone());

        (trader, registry)
    }

    fn make_intent(token: &str, side: Side, price: Decimal, size: Decimal) -> OrderIntent {
        OrderIntent {
            market_id: "market-1".to_string(),
            token_id: token.to_string(),
            side,
            price,
            size,
            urgency: Urgency::Immediate,
            reason: "test arb".to_string(),
            strategy_name: "test".to_string(),
            group_id: Some("arb-1".to_string()),
            priority: 0,
            created_at: Instant::now(),
        }
    }

    fn make_order_book_state() -> OrderBookState {
        let state = OrderBookState::new();

        // YES token book - tighter spread (49-50)
        state.update_book(
            "token-yes".to_string(),
            "market-1".to_string(),
            vec![PriceLevel {
                price: "0.49".to_string(),
                size: "100".to_string(),
            }],
            vec![PriceLevel {
                price: "0.50".to_string(),
                size: "100".to_string(),
            }],
            Some(0),
            Some(String::new()),
        );

        // NO token book - tighter spread (46-47)
        state.update_book(
            "token-no".to_string(),
            "market-1".to_string(),
            vec![PriceLevel {
                price: "0.46".to_string(),
                size: "100".to_string(),
            }],
            vec![PriceLevel {
                price: "0.47".to_string(),
                size: "100".to_string(),
            }],
            Some(0),
            Some(String::new()),
        );

        state
    }

    #[test]
    fn test_process_arb_pair() {
        let (mut trader, _) = setup_trader();
        let order_book = make_order_book_state();

        let intents = vec![
            make_intent("token-yes", Side::Buy, dec!(0.52), dec!(50)),
            make_intent("token-no", Side::Buy, dec!(0.47), dec!(50)),
        ];

        let fills = trader.process_intents(intents, &order_book);

        assert_eq!(fills.len(), 2);

        let stats = trader.stats();
        assert_eq!(stats.opportunities, 1);
        assert_eq!(stats.arb_attempts, 1);
        assert_eq!(stats.successful_arbs, 1);
    }

    #[test]
    fn test_paper_stats() {
        let (mut trader, _) = setup_trader();
        let order_book = make_order_book_state();

        // Process a few arbs
        for i in 0..3 {
            let intents = vec![
                OrderIntent {
                    group_id: Some(format!("arb-{}", i)),
                    ..make_intent("token-yes", Side::Buy, dec!(0.52), dec!(50))
                },
                OrderIntent {
                    group_id: Some(format!("arb-{}", i)),
                    ..make_intent("token-no", Side::Buy, dec!(0.47), dec!(50))
                },
            ];
            trader.process_intents(intents, &order_book);
        }

        let stats = trader.stats();
        assert_eq!(stats.opportunities, 3);
        assert_eq!(stats.arb_attempts, 3);
        assert_eq!(stats.simulated_fills, 6); // 2 per arb
    }

    #[test]
    fn test_maker_vs_taker_pnl() {
        let (mut trader, _) = setup_trader();
        let order_book = make_order_book_state();

        let intents = vec![
            make_intent("token-yes", Side::Buy, dec!(0.50), dec!(80)),  // Only 80 available after discount
            make_intent("token-no", Side::Buy, dec!(0.47), dec!(80)),
        ];

        trader.process_intents(intents, &order_book);

        let stats = trader.stats();

        // Combined cost: $40 + $37.60 = $77.60
        // Edge: $2.40 (80 shares, payout is $80, cost is $77.60)
        // Taker fees: $77.60 * 3% = $2.33
        // Taker P&L: $2.40 - $2.33 = $0.07

        // With 3% edge, taker barely breaks even or makes tiny profit
        // With 0% fees, maker makes full edge
        assert!(stats.simulated_pnl_maker > Decimal::ZERO, "Maker should profit");
        assert!(stats.simulated_pnl_maker > stats.simulated_pnl_taker, "Maker should profit more than taker");
    }
}
