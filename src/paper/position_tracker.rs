//! Position tracker - tracks simulated positions and P&L
//!
//! Maintains a simulated portfolio of positions based on simulated fills.
//! Calculates realized P&L when markets resolve.

use crate::api::types::{ConditionId, Side, TokenId};
use crate::paper::fill_simulator::SimulatedFill;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::time::Instant;

// ============================================================================
// POSITION SIDE
// ============================================================================

/// Which side of a binary market we hold
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    /// Long the YES/Up token
    LongYes,
    /// Long the NO/Down token
    LongNo,
}

impl PositionSide {
    /// Returns the opposite side
    pub fn opposite(&self) -> Self {
        match self {
            PositionSide::LongYes => PositionSide::LongNo,
            PositionSide::LongNo => PositionSide::LongYes,
        }
    }
}

// ============================================================================
// SIMULATED POSITION
// ============================================================================

/// A simulated position in a token
#[derive(Debug, Clone)]
pub struct SimulatedPosition {
    /// Token ID
    pub token_id: TokenId,
    /// Market/condition ID
    pub market_id: ConditionId,
    /// Number of shares held
    pub shares: Decimal,
    /// Average cost per share
    pub avg_cost: Decimal,
    /// Total cost basis
    pub cost_basis: Decimal,
    /// Which side (YES or NO)
    pub side: PositionSide,
    /// When the position was opened
    pub opened_at: Instant,
    /// Simulated fees paid for this position
    pub fees_paid: Decimal,
}

impl SimulatedPosition {
    /// Create a new position from a fill
    pub fn from_fill(fill: &SimulatedFill, market_id: ConditionId, side: PositionSide) -> Self {
        let shares = fill.fill_size.unwrap_or(Decimal::ZERO);
        let price = fill.fill_price.unwrap_or(Decimal::ZERO);
        let cost_basis = shares * price;

        Self {
            token_id: fill.intent.token_id.clone(),
            market_id,
            shares,
            avg_cost: price,
            cost_basis,
            side,
            opened_at: Instant::now(),
            fees_paid: Decimal::ZERO,
        }
    }

    /// Add to existing position
    pub fn add(&mut self, fill: &SimulatedFill) {
        let new_shares = fill.fill_size.unwrap_or(Decimal::ZERO);
        let new_price = fill.fill_price.unwrap_or(Decimal::ZERO);
        let new_cost = new_shares * new_price;

        let total_shares = self.shares + new_shares;
        let total_cost = self.cost_basis + new_cost;

        self.shares = total_shares;
        self.cost_basis = total_cost;
        self.avg_cost = if total_shares > Decimal::ZERO {
            total_cost / total_shares
        } else {
            Decimal::ZERO
        };
    }

    /// Calculate value if this position wins (pays $1 per share)
    pub fn value_if_wins(&self) -> Decimal {
        self.shares
    }

    /// Calculate value if this position loses (pays $0 per share)
    pub fn value_if_loses(&self) -> Decimal {
        Decimal::ZERO
    }

    /// Calculate P&L if position wins
    pub fn pnl_if_wins(&self) -> Decimal {
        self.value_if_wins() - self.cost_basis - self.fees_paid
    }

    /// Calculate P&L if position loses
    pub fn pnl_if_loses(&self) -> Decimal {
        self.value_if_loses() - self.cost_basis - self.fees_paid
    }
}

// ============================================================================
// CLOSED POSITION
// ============================================================================

/// Reason a position was closed
#[derive(Debug, Clone)]
pub enum CloseReason {
    /// Market resolved
    MarketResolution {
        /// Did this position win?
        won: bool,
        /// Winning outcome token
        winning_token: TokenId,
    },
    /// Manually closed
    ManualClose,
    /// Expired without resolution
    Expired,
}

/// A position that has been closed
#[derive(Debug, Clone)]
pub struct ClosedPosition {
    /// The original position
    pub position: SimulatedPosition,
    /// When it was closed
    pub closed_at: Instant,
    /// Why it was closed
    pub close_reason: CloseReason,
    /// Realized P&L
    pub realized_pnl: Decimal,
    /// Payout received (if any)
    pub payout: Decimal,
}

// ============================================================================
// POSITION TRACKER
// ============================================================================

/// Tracks all simulated positions and calculates P&L
pub struct PositionTracker {
    /// Open positions by token ID
    positions: HashMap<TokenId, SimulatedPosition>,

    /// Closed positions (history)
    closed: Vec<ClosedPosition>,

    /// Simulated cash balance
    simulated_cash: Decimal,

    /// Starting capital
    starting_capital: Decimal,

    /// Total realized P&L
    realized_pnl: Decimal,

    /// Total fees paid (simulated)
    total_fees: Decimal,

    /// Taker fee rate for simulation
    taker_fee_rate: Decimal,

    /// Maker fee rate for simulation
    maker_fee_rate: Decimal,

    /// Currently simulating as maker or taker?
    use_maker_fees: bool,
}

impl PositionTracker {
    /// Create a new position tracker
    pub fn new(starting_capital: Decimal) -> Self {
        Self {
            positions: HashMap::new(),
            closed: Vec::new(),
            simulated_cash: starting_capital,
            starting_capital,
            realized_pnl: Decimal::ZERO,
            total_fees: Decimal::ZERO,
            taker_fee_rate: dec!(0.03), // 3%
            maker_fee_rate: Decimal::ZERO, // 0%
            use_maker_fees: false,
        }
    }

    /// Create with custom fee rates
    pub fn with_fees(mut self, taker_rate: Decimal, maker_rate: Decimal) -> Self {
        self.taker_fee_rate = taker_rate;
        self.maker_fee_rate = maker_rate;
        self
    }

    /// Set whether to use maker fees
    pub fn set_maker_mode(&mut self, use_maker: bool) {
        self.use_maker_fees = use_maker;
    }

    /// Get current fee rate based on mode
    pub fn current_fee_rate(&self) -> Decimal {
        if self.use_maker_fees {
            self.maker_fee_rate
        } else {
            self.taker_fee_rate
        }
    }

    /// Record a simulated fill
    pub fn record_fill(&mut self, fill: &SimulatedFill, market_id: &ConditionId, side: PositionSide) {
        if !fill.would_fill() {
            return; // No fill, nothing to record
        }

        let size = fill.fill_size.unwrap_or(Decimal::ZERO);
        let price = fill.fill_price.unwrap_or(Decimal::ZERO);
        let cost = size * price;
        let fee = cost * self.current_fee_rate();

        // Deduct cost and fees from cash
        self.simulated_cash -= cost + fee;
        self.total_fees += fee;

        // Update or create position
        if let Some(pos) = self.positions.get_mut(&fill.intent.token_id) {
            pos.add(fill);
            pos.fees_paid += fee;
        } else {
            let mut pos = SimulatedPosition::from_fill(fill, market_id.clone(), side);
            pos.fees_paid = fee;
            self.positions.insert(fill.intent.token_id.clone(), pos);
        }
    }

    /// Resolve a market - close positions and calculate P&L
    pub fn resolve_market(&mut self, market_id: &ConditionId, winning_token: &TokenId) -> Vec<ClosedPosition> {
        let mut closed_positions = Vec::new();

        // Find all positions for this market
        let tokens_to_close: Vec<TokenId> = self
            .positions
            .values()
            .filter(|p| &p.market_id == market_id)
            .map(|p| p.token_id.clone())
            .collect();

        for token_id in tokens_to_close {
            if let Some(position) = self.positions.remove(&token_id) {
                let won = &position.token_id == winning_token;
                let payout = if won { position.shares } else { Decimal::ZERO };
                let pnl = payout - position.cost_basis - position.fees_paid;

                // Add payout to cash
                self.simulated_cash += payout;
                self.realized_pnl += pnl;

                let closed = ClosedPosition {
                    position,
                    closed_at: Instant::now(),
                    close_reason: CloseReason::MarketResolution {
                        won,
                        winning_token: winning_token.clone(),
                    },
                    realized_pnl: pnl,
                    payout,
                };

                self.closed.push(closed.clone());
                closed_positions.push(closed);
            }
        }

        closed_positions
    }

    /// Get a position by token ID
    pub fn get_position(&self, token_id: &TokenId) -> Option<&SimulatedPosition> {
        self.positions.get(token_id)
    }

    /// Get all open positions
    pub fn open_positions(&self) -> impl Iterator<Item = &SimulatedPosition> {
        self.positions.values()
    }

    /// Get all closed positions
    pub fn closed_positions(&self) -> &[ClosedPosition] {
        &self.closed
    }

    /// Number of open positions
    pub fn open_count(&self) -> usize {
        self.positions.len()
    }

    /// Current simulated cash
    pub fn cash(&self) -> Decimal {
        self.simulated_cash
    }

    /// Total realized P&L
    pub fn realized_pnl(&self) -> Decimal {
        self.realized_pnl
    }

    /// Total fees paid
    pub fn total_fees(&self) -> Decimal {
        self.total_fees
    }

    /// Unrealized P&L (assuming all positions lose - conservative)
    pub fn unrealized_pnl_conservative(&self) -> Decimal {
        self.positions
            .values()
            .map(|p| p.pnl_if_loses())
            .sum()
    }

    /// Total portfolio value (cash + unrealized, conservative)
    pub fn portfolio_value(&self) -> Decimal {
        self.simulated_cash + self.positions.values().map(|p| p.cost_basis).sum::<Decimal>()
    }

    /// Calculate overall P&L from starting capital
    pub fn total_pnl(&self) -> Decimal {
        self.portfolio_value() - self.starting_capital
    }

    /// Get positions for a specific market
    pub fn positions_for_market(&self, market_id: &ConditionId) -> Vec<&SimulatedPosition> {
        self.positions
            .values()
            .filter(|p| &p.market_id == market_id)
            .collect()
    }

    /// Check if we have a position in a token
    pub fn has_position(&self, token_id: &TokenId) -> bool {
        self.positions.contains_key(token_id)
    }

    /// Get summary stats
    pub fn summary(&self) -> PositionSummary {
        PositionSummary {
            open_positions: self.open_count(),
            closed_positions: self.closed.len(),
            starting_capital: self.starting_capital,
            current_cash: self.simulated_cash,
            total_cost_basis: self.positions.values().map(|p| p.cost_basis).sum(),
            realized_pnl: self.realized_pnl,
            total_fees: self.total_fees,
            portfolio_value: self.portfolio_value(),
        }
    }
}

/// Summary of position tracker state
#[derive(Debug, Clone)]
pub struct PositionSummary {
    pub open_positions: usize,
    pub closed_positions: usize,
    pub starting_capital: Decimal,
    pub current_cash: Decimal,
    pub total_cost_basis: Decimal,
    pub realized_pnl: Decimal,
    pub total_fees: Decimal,
    pub portfolio_value: Decimal,
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::fill_simulator::FillOutcome;
    use crate::strategy::{OrderIntent, Urgency};

    fn make_fill(token_id: &str, price: Decimal, size: Decimal) -> SimulatedFill {
        SimulatedFill {
            intent: OrderIntent {
                market_id: "test-market".to_string(),
                token_id: token_id.to_string(),
                side: Side::Buy,
                price,
                size,
                urgency: Urgency::Immediate,
                reason: "test".to_string(),
                strategy_name: "test".to_string(),
                group_id: None,
                priority: 0,
                created_at: Instant::now(),
            },
            timestamp: Instant::now(),
            best_bid: Some(price - dec!(0.05)),
            best_ask: Some(price),
            bid_depth: dec!(100),
            ask_depth: dec!(100),
            spread_bps: 100,
            outcome: FillOutcome::FullFill,
            fill_price: Some(price),
            fill_size: Some(size),
            slippage_cents: Some(Decimal::ZERO),
            group_id: None,
        }
    }

    #[test]
    fn test_record_fill() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(true); // 0% fees

        let fill = make_fill("token-yes", dec!(0.50), dec!(100));
        tracker.record_fill(&fill, &"market-1".to_string(), PositionSide::LongYes);

        assert_eq!(tracker.open_count(), 1);
        assert_eq!(tracker.cash(), dec!(950)); // 1000 - (100 * 0.50)

        let pos = tracker.get_position(&"token-yes".to_string()).unwrap();
        assert_eq!(pos.shares, dec!(100));
        assert_eq!(pos.avg_cost, dec!(0.50));
        assert_eq!(pos.cost_basis, dec!(50));
    }

    #[test]
    fn test_add_to_position() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(true);

        let fill1 = make_fill("token-yes", dec!(0.50), dec!(100));
        tracker.record_fill(&fill1, &"market-1".to_string(), PositionSide::LongYes);

        let fill2 = make_fill("token-yes", dec!(0.60), dec!(100));
        tracker.record_fill(&fill2, &"market-1".to_string(), PositionSide::LongYes);

        let pos = tracker.get_position(&"token-yes".to_string()).unwrap();
        assert_eq!(pos.shares, dec!(200));
        // Avg cost = (100*0.50 + 100*0.60) / 200 = 110 / 200 = 0.55
        assert_eq!(pos.avg_cost, dec!(0.55));
        assert_eq!(pos.cost_basis, dec!(110));
    }

    #[test]
    fn test_resolve_market_win() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(true);

        let fill = make_fill("token-yes", dec!(0.50), dec!(100));
        tracker.record_fill(&fill, &"market-1".to_string(), PositionSide::LongYes);

        // YES wins - we get $100 (100 shares * $1)
        let closed = tracker.resolve_market(&"market-1".to_string(), &"token-yes".to_string());

        assert_eq!(closed.len(), 1);
        assert_eq!(tracker.open_count(), 0);

        let closed_pos = &closed[0];
        assert!(matches!(
            closed_pos.close_reason,
            CloseReason::MarketResolution { won: true, .. }
        ));
        assert_eq!(closed_pos.payout, dec!(100));
        // P&L = payout - cost = 100 - 50 = 50
        assert_eq!(closed_pos.realized_pnl, dec!(50));

        // Cash: started 1000, spent 50, received 100 = 1050
        assert_eq!(tracker.cash(), dec!(1050));
    }

    #[test]
    fn test_resolve_market_lose() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(true);

        let fill = make_fill("token-yes", dec!(0.50), dec!(100));
        tracker.record_fill(&fill, &"market-1".to_string(), PositionSide::LongYes);

        // NO wins - we get $0
        let closed = tracker.resolve_market(&"market-1".to_string(), &"token-no".to_string());

        assert_eq!(closed.len(), 1);
        let closed_pos = &closed[0];
        assert!(matches!(
            closed_pos.close_reason,
            CloseReason::MarketResolution { won: false, .. }
        ));
        assert_eq!(closed_pos.payout, Decimal::ZERO);
        // P&L = payout - cost = 0 - 50 = -50
        assert_eq!(closed_pos.realized_pnl, dec!(-50));

        // Cash: started 1000, spent 50, received 0 = 950
        assert_eq!(tracker.cash(), dec!(950));
    }

    #[test]
    fn test_taker_fees() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(false); // 3% fees

        let fill = make_fill("token-yes", dec!(0.50), dec!(100));
        tracker.record_fill(&fill, &"market-1".to_string(), PositionSide::LongYes);

        // Cost: 100 * 0.50 = $50
        // Fee: $50 * 3% = $1.50
        // Total deducted: $51.50
        assert_eq!(tracker.cash(), dec!(948.50));
        assert_eq!(tracker.total_fees(), dec!(1.50));
    }

    #[test]
    fn test_arb_position() {
        let mut tracker = PositionTracker::new(dec!(1000));
        tracker.set_maker_mode(true);

        // Buy YES at $0.52
        let fill_yes = make_fill("token-yes", dec!(0.52), dec!(100));
        tracker.record_fill(&fill_yes, &"market-1".to_string(), PositionSide::LongYes);

        // Buy NO at $0.47
        let fill_no = make_fill("token-no", dec!(0.47), dec!(100));
        tracker.record_fill(&fill_no, &"market-1".to_string(), PositionSide::LongNo);

        // Total cost: 52 + 47 = $99
        // Cash: 1000 - 99 = 901
        assert_eq!(tracker.cash(), dec!(901));
        assert_eq!(tracker.open_count(), 2);

        // Resolve - YES wins
        tracker.resolve_market(&"market-1".to_string(), &"token-yes".to_string());

        // YES wins: get $100 for YES, $0 for NO
        // Payout: $100
        // Cost: $99
        // P&L: $1
        assert_eq!(tracker.realized_pnl(), dec!(1));
        // Cash: 901 + 100 = 1001
        assert_eq!(tracker.cash(), dec!(1001));
    }
}
