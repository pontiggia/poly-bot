//! Paper analytics - aggregates statistics from paper trading
//!
//! Collects and analyzes all simulated executions, fills, and P&L
//! to provide comprehensive performance metrics.

use crate::paper::fill_simulator::{ArbSimulation, FillOutcome, SimulatedFill};
use crate::paper::position_tracker::PositionSummary;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::{Duration, Instant};

// ============================================================================
// ANALYTICS SUMMARY
// ============================================================================

/// Comprehensive summary of paper trading performance
#[derive(Debug, Clone)]
pub struct AnalyticsSummary {
    // Timing
    /// How long the session has been running
    pub session_duration: Duration,
    /// Total book updates received
    pub book_updates: u64,
    /// Updates per second
    pub updates_per_second: f64,

    // Opportunity detection
    /// Total arb opportunities detected by strategy
    pub opportunities_detected: u64,
    /// Opportunities per hour
    pub opportunities_per_hour: f64,

    // Execution simulation
    /// Total order intents generated
    pub total_intents: u64,
    /// Intents that would fully fill
    pub would_full_fill: u64,
    /// Intents that would partially fill
    pub would_partial_fill: u64,
    /// Intents that would not fill at all
    pub would_not_fill: u64,
    /// Fill rate (full + partial) / total
    pub fill_rate: f64,

    // Arb-specific
    /// Total arb attempts (pairs of orders)
    pub arb_attempts: u64,
    /// Arbs where both legs would fill
    pub arb_both_legs_fill: u64,
    /// Arbs where only one leg would fill (dangerous!)
    pub arb_one_leg_only: u64,
    /// Arbs where neither leg would fill
    pub arb_neither_leg: u64,
    /// Arb success rate (both legs fill)
    pub arb_success_rate: f64,
    /// Partial arb rate (one leg only - risk indicator)
    pub partial_arb_rate: f64,

    // P&L simulation
    /// Gross edge captured before fees
    pub gross_edge_captured: Decimal,
    /// Simulated taker fees (3%)
    pub taker_fees: Decimal,
    /// Simulated maker fees (0%)
    pub maker_fees: Decimal,
    /// Net P&L if trading as taker
    pub net_pnl_taker: Decimal,
    /// Net P&L if trading as maker
    pub net_pnl_maker: Decimal,
    /// Projected daily P&L (taker)
    pub projected_daily_pnl_taker: Decimal,
    /// Projected daily P&L (maker)
    pub projected_daily_pnl_maker: Decimal,

    // Edge analysis
    /// Average edge percentage across all arbs
    pub avg_edge_percent: f64,
    /// Minimum edge seen
    pub min_edge_percent: f64,
    /// Maximum edge seen
    pub max_edge_percent: f64,

    // Slippage analysis
    /// Average slippage in cents
    pub avg_slippage_cents: f64,
    /// Maximum slippage seen
    pub max_slippage_cents: f64,

    // Position summary
    /// Current position state
    pub position_summary: Option<PositionSummary>,

    // Verdict
    /// Is taker mode profitable?
    pub taker_profitable: bool,
    /// Is maker mode profitable?
    pub maker_profitable: bool,
}

impl Default for AnalyticsSummary {
    fn default() -> Self {
        Self {
            session_duration: Duration::ZERO,
            book_updates: 0,
            updates_per_second: 0.0,
            opportunities_detected: 0,
            opportunities_per_hour: 0.0,
            total_intents: 0,
            would_full_fill: 0,
            would_partial_fill: 0,
            would_not_fill: 0,
            fill_rate: 0.0,
            arb_attempts: 0,
            arb_both_legs_fill: 0,
            arb_one_leg_only: 0,
            arb_neither_leg: 0,
            arb_success_rate: 0.0,
            partial_arb_rate: 0.0,
            gross_edge_captured: Decimal::ZERO,
            taker_fees: Decimal::ZERO,
            maker_fees: Decimal::ZERO,
            net_pnl_taker: Decimal::ZERO,
            net_pnl_maker: Decimal::ZERO,
            projected_daily_pnl_taker: Decimal::ZERO,
            projected_daily_pnl_maker: Decimal::ZERO,
            avg_edge_percent: 0.0,
            min_edge_percent: 0.0,
            max_edge_percent: 0.0,
            avg_slippage_cents: 0.0,
            max_slippage_cents: 0.0,
            position_summary: None,
            taker_profitable: false,
            maker_profitable: false,
        }
    }
}

// ============================================================================
// PAPER ANALYTICS
// ============================================================================

/// Aggregates and analyzes paper trading data
pub struct PaperAnalytics {
    /// Session start time
    started_at: Instant,

    /// All simulated fills
    fills: Vec<SimulatedFill>,

    /// All arb simulations
    arbs: Vec<ArbSimulation>,

    /// Book update counter
    book_updates: u64,

    /// Opportunity counter (from strategy)
    opportunities_detected: u64,

    /// Taker fee rate for calculations
    taker_fee_rate: Decimal,

    /// Maker fee rate for calculations
    maker_fee_rate: Decimal,
}

impl PaperAnalytics {
    /// Create a new analytics tracker
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            fills: Vec::new(),
            arbs: Vec::new(),
            book_updates: 0,
            opportunities_detected: 0,
            taker_fee_rate: dec!(0.03),
            maker_fee_rate: Decimal::ZERO,
        }
    }

    /// Set fee rates for calculations
    pub fn with_fees(mut self, taker_rate: Decimal, maker_rate: Decimal) -> Self {
        self.taker_fee_rate = taker_rate;
        self.maker_fee_rate = maker_rate;
        self
    }

    /// Record a book update
    pub fn record_book_update(&mut self) {
        self.book_updates += 1;
    }

    /// Record an opportunity detection (from strategy)
    pub fn record_opportunity(&mut self) {
        self.opportunities_detected += 1;
    }

    /// Record a simulated fill
    pub fn record_fill(&mut self, fill: SimulatedFill) {
        self.fills.push(fill);
    }

    /// Record an arb simulation
    pub fn record_arb(&mut self, arb: ArbSimulation) {
        self.arbs.push(arb);
    }

    /// Get number of book updates
    pub fn book_updates(&self) -> u64 {
        self.book_updates
    }

    /// Get number of opportunities detected
    pub fn opportunities(&self) -> u64 {
        self.opportunities_detected
    }

    /// Get number of arb attempts
    pub fn arb_attempts(&self) -> usize {
        self.arbs.len()
    }

    /// Get number of fills
    pub fn fill_count(&self) -> usize {
        self.fills.len()
    }

    /// Calculate comprehensive summary
    pub fn summary(&self, position_summary: Option<PositionSummary>) -> AnalyticsSummary {
        let session_duration = self.started_at.elapsed();
        let session_hours = session_duration.as_secs_f64() / 3600.0;
        let session_secs = session_duration.as_secs_f64().max(1.0);

        // Timing stats
        let updates_per_second = self.book_updates as f64 / session_secs;
        let opportunities_per_hour = if session_hours > 0.0 {
            self.opportunities_detected as f64 / session_hours
        } else {
            0.0
        };

        // Fill simulation stats
        let total_intents = self.fills.len() as u64;
        let mut would_full_fill = 0u64;
        let mut would_partial_fill = 0u64;
        let mut would_not_fill = 0u64;

        for fill in &self.fills {
            match &fill.outcome {
                FillOutcome::FullFill => would_full_fill += 1,
                FillOutcome::PartialFill { .. } => would_partial_fill += 1,
                FillOutcome::NoFill { .. } | FillOutcome::Rejected { .. } => would_not_fill += 1,
            }
        }

        let fill_rate = if total_intents > 0 {
            (would_full_fill + would_partial_fill) as f64 / total_intents as f64
        } else {
            0.0
        };

        // Arb stats
        let arb_attempts = self.arbs.len() as u64;
        let mut arb_both_legs_fill = 0u64;
        let mut arb_one_leg_only = 0u64;
        let mut arb_neither_leg = 0u64;

        for arb in &self.arbs {
            if arb.both_would_fill {
                arb_both_legs_fill += 1;
            } else if arb.partial_arb {
                arb_one_leg_only += 1;
            } else {
                arb_neither_leg += 1;
            }
        }

        let arb_success_rate = if arb_attempts > 0 {
            arb_both_legs_fill as f64 / arb_attempts as f64
        } else {
            0.0
        };

        let partial_arb_rate = if arb_attempts > 0 {
            arb_one_leg_only as f64 / arb_attempts as f64
        } else {
            0.0
        };

        // P&L calculations (from successful arbs only)
        let successful_arbs: Vec<&ArbSimulation> =
            self.arbs.iter().filter(|a| a.both_would_fill).collect();

        let gross_edge_captured: Decimal = successful_arbs.iter().map(|a| a.gross_edge).sum();
        let taker_fees: Decimal = successful_arbs
            .iter()
            .map(|a| a.combined_cost * self.taker_fee_rate)
            .sum();
        let maker_fees: Decimal = successful_arbs
            .iter()
            .map(|a| a.combined_cost * self.maker_fee_rate)
            .sum();

        let net_pnl_taker = gross_edge_captured - taker_fees;
        let net_pnl_maker = gross_edge_captured - maker_fees;

        // Project to daily (24 hours)
        let hours_elapsed = session_hours.max(0.001); // Avoid division by zero
        let projected_daily_pnl_taker = net_pnl_taker * Decimal::from_f64_retain(24.0 / hours_elapsed).unwrap_or(dec!(1));
        let projected_daily_pnl_maker = net_pnl_maker * Decimal::from_f64_retain(24.0 / hours_elapsed).unwrap_or(dec!(1));

        // Edge analysis
        let edges: Vec<f64> = successful_arbs
            .iter()
            .filter_map(|a| a.gross_edge_percent.to_string().parse::<f64>().ok())
            .collect();

        let (avg_edge_percent, min_edge_percent, max_edge_percent) = if !edges.is_empty() {
            let sum: f64 = edges.iter().sum();
            let avg = sum / edges.len() as f64;
            let min = edges.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = edges.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            (avg, min, max)
        } else {
            (0.0, 0.0, 0.0)
        };

        // Slippage analysis
        let slippages: Vec<f64> = self
            .fills
            .iter()
            .filter_map(|f| f.slippage_cents.and_then(|s| s.to_string().parse::<f64>().ok()))
            .collect();

        let (avg_slippage_cents, max_slippage_cents) = if !slippages.is_empty() {
            let sum: f64 = slippages.iter().sum();
            let avg = sum / slippages.len() as f64;
            let max = slippages.iter().cloned().fold(0.0, f64::max);
            (avg, max)
        } else {
            (0.0, 0.0)
        };

        // Verdict
        let taker_profitable = net_pnl_taker > Decimal::ZERO;
        let maker_profitable = net_pnl_maker > Decimal::ZERO;

        AnalyticsSummary {
            session_duration,
            book_updates: self.book_updates,
            updates_per_second,
            opportunities_detected: self.opportunities_detected,
            opportunities_per_hour,
            total_intents,
            would_full_fill,
            would_partial_fill,
            would_not_fill,
            fill_rate,
            arb_attempts,
            arb_both_legs_fill,
            arb_one_leg_only,
            arb_neither_leg,
            arb_success_rate,
            partial_arb_rate,
            gross_edge_captured,
            taker_fees,
            maker_fees,
            net_pnl_taker,
            net_pnl_maker,
            projected_daily_pnl_taker,
            projected_daily_pnl_maker,
            avg_edge_percent,
            min_edge_percent,
            max_edge_percent,
            avg_slippage_cents,
            max_slippage_cents,
            position_summary,
            taker_profitable,
            maker_profitable,
        }
    }

    /// Get quick stats for heartbeat logging
    pub fn quick_stats(&self) -> QuickStats {
        let successful_arbs = self.arbs.iter().filter(|a| a.both_would_fill).count();
        let partial_arbs = self.arbs.iter().filter(|a| a.partial_arb).count();

        let gross_pnl: Decimal = self
            .arbs
            .iter()
            .filter(|a| a.both_would_fill)
            .map(|a| a.gross_edge)
            .sum();

        let net_pnl_maker = gross_pnl; // 0% fees
        let taker_costs: Decimal = self
            .arbs
            .iter()
            .filter(|a| a.both_would_fill)
            .map(|a| a.combined_cost * self.taker_fee_rate)
            .sum();
        let net_pnl_taker = gross_pnl - taker_costs;

        QuickStats {
            opportunities: self.opportunities_detected,
            arb_attempts: self.arbs.len() as u64,
            successful_arbs: successful_arbs as u64,
            partial_arbs: partial_arbs as u64,
            net_pnl_taker,
            net_pnl_maker,
        }
    }

    /// Clear all data (for fresh start)
    pub fn reset(&mut self) {
        self.started_at = Instant::now();
        self.fills.clear();
        self.arbs.clear();
        self.book_updates = 0;
        self.opportunities_detected = 0;
    }
}

impl Default for PaperAnalytics {
    fn default() -> Self {
        Self::new()
    }
}

/// Quick stats for heartbeat logging
#[derive(Debug, Clone)]
pub struct QuickStats {
    pub opportunities: u64,
    pub arb_attempts: u64,
    pub successful_arbs: u64,
    pub partial_arbs: u64,
    pub net_pnl_taker: Decimal,
    pub net_pnl_maker: Decimal,
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Side;
    use crate::paper::fill_simulator::SimulatedFill;
    use crate::strategy::{OrderIntent, Urgency};

    fn make_fill(outcome: FillOutcome, slippage: Option<Decimal>) -> SimulatedFill {
        SimulatedFill {
            intent: OrderIntent {
                market_id: "test".to_string(),
                token_id: "test".to_string(),
                side: Side::Buy,
                price: dec!(0.50),
                size: dec!(100),
                urgency: Urgency::Immediate,
                reason: "test".to_string(),
                strategy_name: "test".to_string(),
                group_id: None,
                priority: 0,
                created_at: Instant::now(),
            },
            timestamp: Instant::now(),
            best_bid: Some(dec!(0.48)),
            best_ask: Some(dec!(0.50)),
            bid_depth: dec!(100),
            ask_depth: dec!(100),
            spread_bps: 40,
            outcome,
            fill_price: Some(dec!(0.50)),
            fill_size: Some(dec!(100)),
            slippage_cents: slippage,
            group_id: None,
        }
    }

    fn make_arb(both_fill: bool, edge: Decimal) -> ArbSimulation {
        ArbSimulation {
            leg1: make_fill(FillOutcome::FullFill, Some(Decimal::ZERO)),
            leg2: make_fill(FillOutcome::FullFill, Some(Decimal::ZERO)),
            group_id: "arb-1".to_string(),
            timestamp: Instant::now(),
            both_would_fill: both_fill,
            partial_arb: false,
            combined_cost: dec!(99),
            gross_edge: edge,
            gross_edge_percent: edge * dec!(100) / dec!(99),
            net_pnl_taker: edge - (dec!(99) * dec!(0.03)),
            net_pnl_maker: edge,
        }
    }

    #[test]
    fn test_analytics_fill_tracking() {
        let mut analytics = PaperAnalytics::new();

        analytics.record_fill(make_fill(FillOutcome::FullFill, Some(Decimal::ZERO)));
        analytics.record_fill(make_fill(
            FillOutcome::PartialFill {
                available_size: dec!(50),
            },
            Some(dec!(0.5)),
        ));
        analytics.record_fill(make_fill(
            FillOutcome::NoFill {
                reason: crate::paper::fill_simulator::NoFillReason::NoLiquidity,
            },
            None,
        ));

        let summary = analytics.summary(None);

        assert_eq!(summary.total_intents, 3);
        assert_eq!(summary.would_full_fill, 1);
        assert_eq!(summary.would_partial_fill, 1);
        assert_eq!(summary.would_not_fill, 1);
        assert!((summary.fill_rate - 0.6666).abs() < 0.01);
    }

    #[test]
    fn test_analytics_arb_tracking() {
        let mut analytics = PaperAnalytics::new();

        // 3 successful arbs with $1 edge each
        analytics.record_arb(make_arb(true, dec!(1)));
        analytics.record_arb(make_arb(true, dec!(1)));
        analytics.record_arb(make_arb(true, dec!(1)));
        // 1 failed arb
        analytics.record_arb(make_arb(false, dec!(0)));

        let summary = analytics.summary(None);

        assert_eq!(summary.arb_attempts, 4);
        assert_eq!(summary.arb_both_legs_fill, 3);
        assert!((summary.arb_success_rate - 0.75).abs() < 0.01);

        // Gross edge: $3
        assert_eq!(summary.gross_edge_captured, dec!(3));

        // Taker fees: $99 * 3% * 3 arbs = $8.91
        // Net taker: $3 - $8.91 = -$5.91
        assert!(summary.net_pnl_taker < Decimal::ZERO);

        // Net maker: $3 (no fees)
        assert_eq!(summary.net_pnl_maker, dec!(3));
    }

    #[test]
    fn test_quick_stats() {
        let mut analytics = PaperAnalytics::new();
        analytics.record_opportunity();
        analytics.record_opportunity();
        analytics.record_arb(make_arb(true, dec!(2)));

        let stats = analytics.quick_stats();

        assert_eq!(stats.opportunities, 2);
        assert_eq!(stats.successful_arbs, 1);
        assert_eq!(stats.net_pnl_maker, dec!(2));
    }
}
