//! Fill simulator - realistically simulates order execution
//!
//! Given an OrderIntent and current book state, determines:
//! - Would the order fill?
//! - At what price?
//! - How much slippage?
//! - For arb pairs: would both legs fill?

use crate::api::types::Side;
use crate::state::BookSnapshot;
use crate::strategy::{OrderIntent, Urgency};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::time::Instant;

// ============================================================================
// FILL OUTCOME
// ============================================================================

/// Result of simulating whether an order would fill
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FillOutcome {
    /// Order would fill completely at simulated price
    FullFill,

    /// Order would partially fill (available liquidity < requested size)
    PartialFill {
        /// Size that would fill
        available_size: Decimal,
    },

    /// Order would not fill
    NoFill {
        /// Reason for no fill
        reason: NoFillReason,
    },

    /// Order would be rejected before reaching the book
    Rejected {
        /// Reason for rejection
        reason: String,
    },
}

/// Reasons why an order wouldn't fill
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoFillReason {
    /// No liquidity at the required price level
    NoLiquidity,
    /// Price moved away before execution
    PriceMoved { current_best: Decimal },
    /// Spread too wide
    SpreadTooWide { spread_bps: u32 },
    /// Book is empty on the side we need
    EmptyBook,
    /// FOK order couldn't fill entirely
    FokPartialReject { available: Decimal },
}

impl std::fmt::Display for NoFillReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoFillReason::NoLiquidity => write!(f, "No liquidity at price"),
            NoFillReason::PriceMoved { current_best } => {
                write!(f, "Price moved to {}", current_best)
            }
            NoFillReason::SpreadTooWide { spread_bps } => {
                write!(f, "Spread too wide: {} bps", spread_bps)
            }
            NoFillReason::EmptyBook => write!(f, "Order book empty"),
            NoFillReason::FokPartialReject { available } => {
                write!(f, "FOK rejected: only {} available", available)
            }
        }
    }
}

// ============================================================================
// SIMULATED FILL
// ============================================================================

/// Complete record of a simulated fill attempt
#[derive(Debug, Clone)]
pub struct SimulatedFill {
    // Intent details
    /// The original order intent
    pub intent: OrderIntent,
    /// When the simulation was performed
    pub timestamp: Instant,

    // Book state at simulation time
    /// Best bid price at simulation time
    pub best_bid: Option<Decimal>,
    /// Best ask price at simulation time
    pub best_ask: Option<Decimal>,
    /// Total bid depth (liquidity)
    pub bid_depth: Decimal,
    /// Total ask depth (liquidity)
    pub ask_depth: Decimal,
    /// Spread in basis points
    pub spread_bps: u32,

    // Simulation result
    /// Whether the order would fill
    pub outcome: FillOutcome,
    /// Price at which it would fill (if any)
    pub fill_price: Option<Decimal>,
    /// Size that would fill (if any)
    pub fill_size: Option<Decimal>,
    /// Slippage from intent price in cents (positive = worse)
    pub slippage_cents: Option<Decimal>,

    // Arb tracking
    /// Group ID for linked orders (arb legs)
    pub group_id: Option<String>,
}

impl SimulatedFill {
    /// Check if this fill would be successful (full or partial)
    pub fn would_fill(&self) -> bool {
        matches!(self.outcome, FillOutcome::FullFill | FillOutcome::PartialFill { .. })
    }

    /// Get the fill value (price * size) if filled
    pub fn fill_value(&self) -> Option<Decimal> {
        match (&self.fill_price, &self.fill_size) {
            (Some(price), Some(size)) => Some(price * size),
            _ => None,
        }
    }

    /// Calculate simulated fee based on role
    pub fn simulated_fee(&self, fee_rate: Decimal) -> Decimal {
        self.fill_value().map(|v| v * fee_rate).unwrap_or(Decimal::ZERO)
    }
}

// ============================================================================
// ARB SIMULATION
// ============================================================================

/// Simulation of an arbitrage pair (both legs)
#[derive(Debug, Clone)]
pub struct ArbSimulation {
    /// First leg (e.g., YES token)
    pub leg1: SimulatedFill,
    /// Second leg (e.g., NO token)
    pub leg2: SimulatedFill,
    /// Group ID linking the legs
    pub group_id: String,
    /// Timestamp of simulation
    pub timestamp: Instant,

    // Combined analysis
    /// Would both legs fill?
    pub both_would_fill: bool,
    /// Would only one leg fill? (dangerous!)
    pub partial_arb: bool,
    /// Combined cost if both fill
    pub combined_cost: Decimal,
    /// Gross edge (before fees)
    pub gross_edge: Decimal,
    /// Gross edge as percentage
    pub gross_edge_percent: Decimal,
    /// Net P&L with taker fees (3%)
    pub net_pnl_taker: Decimal,
    /// Net P&L with maker fees (0%)
    pub net_pnl_maker: Decimal,
}

impl ArbSimulation {
    /// Create a new arb simulation from two fills
    pub fn new(leg1: SimulatedFill, leg2: SimulatedFill, taker_fee_rate: Decimal) -> Self {
        let group_id = leg1.group_id.clone().unwrap_or_else(|| "unknown".to_string());
        let timestamp = Instant::now();

        let leg1_fills = leg1.would_fill();
        let leg2_fills = leg2.would_fill();

        let both_would_fill = leg1_fills && leg2_fills;
        let partial_arb = leg1_fills != leg2_fills; // XOR - exactly one fills

        // Calculate combined cost and edge
        let (combined_cost, gross_edge, gross_edge_percent) = if both_would_fill {
            let leg1_value = leg1.fill_value().unwrap_or(Decimal::ZERO);
            let leg2_value = leg2.fill_value().unwrap_or(Decimal::ZERO);
            let combined = leg1_value + leg2_value;

            // For binary markets: one side always pays $1 per share
            // If we buy X shares of YES and X shares of NO, we get $X at resolution
            let shares = leg1.fill_size.unwrap_or(Decimal::ZERO)
                .min(leg2.fill_size.unwrap_or(Decimal::ZERO));
            let payout = shares; // $1 per share on winning side
            let edge = payout - combined;
            let edge_pct = if combined > Decimal::ZERO {
                (edge / combined) * dec!(100)
            } else {
                Decimal::ZERO
            };

            (combined, edge, edge_pct)
        } else {
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        };

        // Calculate net P&L with fees
        let taker_fees = combined_cost * taker_fee_rate;
        let net_pnl_taker = gross_edge - taker_fees;
        let net_pnl_maker = gross_edge; // 0% maker fee

        Self {
            leg1,
            leg2,
            group_id,
            timestamp,
            both_would_fill,
            partial_arb,
            combined_cost,
            gross_edge,
            gross_edge_percent,
            net_pnl_taker,
            net_pnl_maker,
        }
    }

    /// Is this arb profitable as a taker?
    pub fn profitable_as_taker(&self) -> bool {
        self.both_would_fill && self.net_pnl_taker > Decimal::ZERO
    }

    /// Is this arb profitable as a maker?
    pub fn profitable_as_maker(&self) -> bool {
        self.both_would_fill && self.net_pnl_maker > Decimal::ZERO
    }

    /// Get the minimum fill size across both legs
    pub fn min_fill_size(&self) -> Decimal {
        let leg1_size = self.leg1.fill_size.unwrap_or(Decimal::ZERO);
        let leg2_size = self.leg2.fill_size.unwrap_or(Decimal::ZERO);
        leg1_size.min(leg2_size)
    }
}

// ============================================================================
// FILL SIMULATOR
// ============================================================================

/// Configuration for the fill simulator
#[derive(Debug, Clone)]
pub struct FillSimulatorConfig {
    /// Simulated network/processing latency in milliseconds
    /// During this time, the book could change
    pub simulated_latency_ms: u64,

    /// Assume this percentage of displayed liquidity is actually available
    /// (accounts for stale quotes, phantom liquidity)
    pub liquidity_discount: Decimal,

    /// Maximum spread (bps) to consider for fills
    /// Orders in very wide spreads are more likely to not fill
    pub max_spread_bps: u32,
}

impl Default for FillSimulatorConfig {
    fn default() -> Self {
        Self {
            simulated_latency_ms: 50, // 50ms realistic latency
            liquidity_discount: dec!(0.8), // Assume 80% of displayed liquidity is real
            max_spread_bps: 1000, // 10% max spread
        }
    }
}

/// Simulates order fills based on book state
pub struct FillSimulator {
    config: FillSimulatorConfig,
}

impl FillSimulator {
    /// Create a new fill simulator with default config
    pub fn new() -> Self {
        Self {
            config: FillSimulatorConfig::default(),
        }
    }

    /// Create with custom config
    pub fn with_config(config: FillSimulatorConfig) -> Self {
        Self { config }
    }

    /// Simulate a single order intent against the current book
    pub fn simulate(&self, intent: &OrderIntent, book: &BookSnapshot) -> SimulatedFill {
        let timestamp = Instant::now();

        // Extract book state - parse strings to Decimal
        let best_bid = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let bid_depth = self.calculate_depth(&book.bids);
        let ask_depth = self.calculate_depth(&book.asks);
        let spread_bps = self.calculate_spread_bps(best_bid, best_ask);

        // Check for empty book
        if (intent.side == Side::Buy && book.asks.is_empty())
            || (intent.side == Side::Sell && book.bids.is_empty())
        {
            return SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::NoFill {
                    reason: NoFillReason::EmptyBook,
                },
                fill_price: None,
                fill_size: None,
                slippage_cents: None,
                group_id: intent.group_id.clone(),
            };
        }

        // Check spread
        if spread_bps > self.config.max_spread_bps {
            return SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::NoFill {
                    reason: NoFillReason::SpreadTooWide { spread_bps },
                },
                fill_price: None,
                fill_size: None,
                slippage_cents: None,
                group_id: intent.group_id.clone(),
            };
        }

        // Simulate based on urgency (which determines order type)
        match intent.urgency {
            Urgency::Immediate => self.simulate_fok(intent, book, timestamp),
            Urgency::Normal => self.simulate_fak(intent, book, timestamp),
            Urgency::Passive => self.simulate_gtc(intent, book, timestamp),
        }
    }

    /// Simulate FOK (Fill Or Kill) - must fill entirely or cancel
    fn simulate_fok(
        &self,
        intent: &OrderIntent,
        book: &BookSnapshot,
        timestamp: Instant,
    ) -> SimulatedFill {
        let best_bid = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let bid_depth = self.calculate_depth(&book.bids);
        let ask_depth = self.calculate_depth(&book.asks);
        let spread_bps = self.calculate_spread_bps(best_bid, best_ask);

        // Calculate available liquidity at our price or better
        let (available_size, fill_price) = match intent.side {
            Side::Buy => {
                let available = self.liquidity_at_or_below(&book.asks, intent.price);
                let price = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
                (available, price)
            }
            Side::Sell => {
                let available = self.liquidity_at_or_above(&book.bids, intent.price);
                let price = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
                (available, price)
            }
        };

        // Apply liquidity discount
        let discounted_available = available_size * self.config.liquidity_discount;

        // FOK: must fill entirely
        if discounted_available >= intent.size {
            let slippage = fill_price.map(|fp| {
                match intent.side {
                    Side::Buy => (fp - intent.price) * dec!(100), // In cents
                    Side::Sell => (intent.price - fp) * dec!(100),
                }
            });

            SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::FullFill,
                fill_price,
                fill_size: Some(intent.size),
                slippage_cents: slippage,
                group_id: intent.group_id.clone(),
            }
        } else {
            SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::NoFill {
                    reason: NoFillReason::FokPartialReject {
                        available: discounted_available,
                    },
                },
                fill_price: None,
                fill_size: None,
                slippage_cents: None,
                group_id: intent.group_id.clone(),
            }
        }
    }

    /// Simulate FAK (Fill And Kill) - fill what's available, cancel rest
    fn simulate_fak(
        &self,
        intent: &OrderIntent,
        book: &BookSnapshot,
        timestamp: Instant,
    ) -> SimulatedFill {
        let best_bid = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let bid_depth = self.calculate_depth(&book.bids);
        let ask_depth = self.calculate_depth(&book.asks);
        let spread_bps = self.calculate_spread_bps(best_bid, best_ask);

        // Calculate available liquidity at our price or better
        let (available_size, fill_price) = match intent.side {
            Side::Buy => {
                let available = self.liquidity_at_or_below(&book.asks, intent.price);
                let price = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
                (available, price)
            }
            Side::Sell => {
                let available = self.liquidity_at_or_above(&book.bids, intent.price);
                let price = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
                (available, price)
            }
        };

        // Apply liquidity discount
        let discounted_available = available_size * self.config.liquidity_discount;

        if discounted_available <= Decimal::ZERO {
            return SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::NoFill {
                    reason: NoFillReason::NoLiquidity,
                },
                fill_price: None,
                fill_size: None,
                slippage_cents: None,
                group_id: intent.group_id.clone(),
            };
        }

        let actual_fill_size = discounted_available.min(intent.size);
        let slippage = fill_price.map(|fp| match intent.side {
            Side::Buy => (fp - intent.price) * dec!(100),
            Side::Sell => (intent.price - fp) * dec!(100),
        });

        let outcome = if actual_fill_size >= intent.size {
            FillOutcome::FullFill
        } else {
            FillOutcome::PartialFill {
                available_size: actual_fill_size,
            }
        };

        SimulatedFill {
            intent: intent.clone(),
            timestamp,
            best_bid,
            best_ask,
            bid_depth,
            ask_depth,
            spread_bps,
            outcome,
            fill_price,
            fill_size: Some(actual_fill_size),
            slippage_cents: slippage,
            group_id: intent.group_id.clone(),
        }
    }

    /// Simulate GTC (Good Till Cancel) - posts on book, fills if price crosses
    fn simulate_gtc(
        &self,
        intent: &OrderIntent,
        book: &BookSnapshot,
        timestamp: Instant,
    ) -> SimulatedFill {
        let best_bid = book.bids.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let best_ask = book.asks.first().and_then(|l| l.price.parse::<Decimal>().ok());
        let bid_depth = self.calculate_depth(&book.bids);
        let ask_depth = self.calculate_depth(&book.asks);
        let spread_bps = self.calculate_spread_bps(best_bid, best_ask);

        // For GTC (maker), we're posting on the book
        // Simulate fill if our price is marketable (crosses the spread)
        let would_cross = match intent.side {
            Side::Buy => best_ask.map(|ask| intent.price >= ask).unwrap_or(false),
            Side::Sell => best_bid.map(|bid| intent.price <= bid).unwrap_or(false),
        };

        if would_cross {
            // Price is marketable - would fill like a taker
            // But for maker strategy, we want to post, not cross
            // Simulate as if we posted and got filled (optimistic for maker)
            let fill_price = Some(intent.price);
            let slippage = Some(Decimal::ZERO); // No slippage when maker

            SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::FullFill,
                fill_price,
                fill_size: Some(intent.size),
                slippage_cents: slippage,
                group_id: intent.group_id.clone(),
            }
        } else {
            // Would post on book - for simulation, assume it fills eventually
            // This is optimistic; real GTC might not fill before market close
            // TODO: Track time and simulate based on book movement
            let fill_price = Some(intent.price);

            SimulatedFill {
                intent: intent.clone(),
                timestamp,
                best_bid,
                best_ask,
                bid_depth,
                ask_depth,
                spread_bps,
                outcome: FillOutcome::FullFill, // Optimistic assumption
                fill_price,
                fill_size: Some(intent.size),
                slippage_cents: Some(Decimal::ZERO),
                group_id: intent.group_id.clone(),
            }
        }
    }

    /// Simulate an arb pair (two linked orders)
    pub fn simulate_arb(
        &self,
        leg1: &OrderIntent,
        leg2: &OrderIntent,
        book1: &BookSnapshot,
        book2: &BookSnapshot,
        taker_fee_rate: Decimal,
    ) -> ArbSimulation {
        let fill1 = self.simulate(leg1, book1);
        let fill2 = self.simulate(leg2, book2);
        ArbSimulation::new(fill1, fill2, taker_fee_rate)
    }

    // Helper methods

    fn calculate_depth(&self, levels: &[crate::api::types::PriceLevel]) -> Decimal {
        levels
            .iter()
            .filter_map(|l| l.size.parse::<Decimal>().ok())
            .sum()
    }

    fn calculate_spread_bps(&self, bid: Option<Decimal>, ask: Option<Decimal>) -> u32 {
        match (bid, ask) {
            (Some(b), Some(a)) if b > Decimal::ZERO => {
                let spread = a - b;
                let mid = (a + b) / dec!(2);
                ((spread / mid) * dec!(10000))
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0) as u32
            }
            _ => 0,
        }
    }

    fn liquidity_at_or_below(
        &self,
        asks: &[crate::api::types::PriceLevel],
        max_price: Decimal,
    ) -> Decimal {
        asks.iter()
            .filter_map(|l| {
                let price = l.price.parse::<Decimal>().ok()?;
                let size = l.size.parse::<Decimal>().ok()?;
                if price <= max_price {
                    Some(size)
                } else {
                    None
                }
            })
            .sum()
    }

    fn liquidity_at_or_above(
        &self,
        bids: &[crate::api::types::PriceLevel],
        min_price: Decimal,
    ) -> Decimal {
        bids.iter()
            .filter_map(|l| {
                let price = l.price.parse::<Decimal>().ok()?;
                let size = l.size.parse::<Decimal>().ok()?;
                if price >= min_price {
                    Some(size)
                } else {
                    None
                }
            })
            .sum()
    }
}

impl Default for FillSimulator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::PriceLevel;
    use rust_decimal_macros::dec;

    fn make_book(bids: Vec<(Decimal, Decimal)>, asks: Vec<(Decimal, Decimal)>) -> BookSnapshot {
        BookSnapshot {
            token_id: "test-token".to_string(),
            market: "test-market".to_string(),
            bids: bids
                .into_iter()
                .map(|(price, size)| PriceLevel { 
                    price: price.to_string(), 
                    size: size.to_string() 
                })
                .collect(),
            asks: asks
                .into_iter()
                .map(|(price, size)| PriceLevel { 
                    price: price.to_string(), 
                    size: size.to_string() 
                })
                .collect(),
            last_update: Some(0),
            hash: Some(String::new()),
        }
    }

    fn make_intent(side: Side, price: Decimal, size: Decimal, urgency: Urgency) -> OrderIntent {
        OrderIntent {
            market_id: "test-market".to_string(),
            token_id: "test-token".to_string(),
            side,
            price,
            size,
            urgency,
            reason: "test".to_string(),
            strategy_name: "test-strategy".to_string(),
            group_id: None,
            priority: 0,
            created_at: Instant::now(),
        }
    }

    #[test]
    fn test_fok_full_fill() {
        let sim = FillSimulator::new();
        let book = make_book(
            vec![(dec!(0.48), dec!(100))], // Tighter spread: 48-50 = 2 cents
            vec![(dec!(0.50), dec!(100))],
        );
        let intent = make_intent(Side::Buy, dec!(0.50), dec!(50), Urgency::Immediate);

        let result = sim.simulate(&intent, &book);
        assert!(matches!(result.outcome, FillOutcome::FullFill));
        assert_eq!(result.fill_size, Some(dec!(50)));
    }

    #[test]
    fn test_fok_insufficient_liquidity() {
        let sim = FillSimulator::new();
        let book = make_book(
            vec![(dec!(0.48), dec!(100))], // Tighter spread
            vec![(dec!(0.50), dec!(20))], // Only 20 available
        );
        // Request 50, but only 16 available after 80% discount
        let intent = make_intent(Side::Buy, dec!(0.50), dec!(50), Urgency::Immediate);

        let result = sim.simulate(&intent, &book);
        assert!(matches!(
            result.outcome,
            FillOutcome::NoFill { reason: NoFillReason::FokPartialReject { .. } }
        ));
    }

    #[test]
    fn test_fak_partial_fill() {
        let sim = FillSimulator::new();
        let book = make_book(
            vec![(dec!(0.48), dec!(100))], // Tighter spread
            vec![(dec!(0.50), dec!(30))], // 30 available, 24 after discount
        );
        let intent = make_intent(Side::Buy, dec!(0.50), dec!(50), Urgency::Normal);

        let result = sim.simulate(&intent, &book);
        assert!(matches!(result.outcome, FillOutcome::PartialFill { .. }));
        assert!(result.fill_size.unwrap() < dec!(50));
    }

    #[test]
    fn test_empty_book() {
        let sim = FillSimulator::new();
        let book = make_book(vec![(dec!(0.45), dec!(100))], vec![]); // No asks
        let intent = make_intent(Side::Buy, dec!(0.50), dec!(50), Urgency::Immediate);

        let result = sim.simulate(&intent, &book);
        assert!(matches!(
            result.outcome,
            FillOutcome::NoFill { reason: NoFillReason::EmptyBook }
        ));
    }

    #[test]
    fn test_arb_simulation() {
        let sim = FillSimulator::new();
        
        // YES book: ask at $0.52
        let book_yes = make_book(
            vec![(dec!(0.50), dec!(100))],
            vec![(dec!(0.52), dec!(100))],
        );
        
        // NO book: ask at $0.47
        let book_no = make_book(
            vec![(dec!(0.45), dec!(100))],
            vec![(dec!(0.47), dec!(100))],
        );

        let leg1 = make_intent(Side::Buy, dec!(0.52), dec!(50), Urgency::Immediate);
        let leg2 = OrderIntent {
            token_id: "no-token".to_string(),
            group_id: Some("arb-1".to_string()),
            ..make_intent(Side::Buy, dec!(0.47), dec!(50), Urgency::Immediate)
        };

        let arb = sim.simulate_arb(&leg1, &leg2, &book_yes, &book_no, dec!(0.03));

        assert!(arb.both_would_fill);
        assert!(!arb.partial_arb);
        // Combined cost: 50 * $0.52 + 50 * $0.47 = $26 + $23.50 = $49.50
        // Payout: $50 (one side wins)
        // Gross edge: $0.50
        assert!(arb.gross_edge > Decimal::ZERO);
        // Taker fees: $49.50 * 3% = $1.485
        // Net taker: $0.50 - $1.485 = -$0.985 (LOSS)
        assert!(arb.net_pnl_taker < Decimal::ZERO);
        // Net maker: $0.50 - $0 = $0.50 (PROFIT)
        assert!(arb.net_pnl_maker > Decimal::ZERO);
    }

    #[test]
    fn test_gtc_simulation() {
        let sim = FillSimulator::new();
        let book = make_book(
            vec![(dec!(0.48), dec!(100))], // Tighter spread: 48-50
            vec![(dec!(0.50), dec!(100))],
        );
        // GTC order inside spread
        let intent = make_intent(Side::Buy, dec!(0.49), dec!(50), Urgency::Passive);

        let result = sim.simulate(&intent, &book);
        // GTC inside spread - optimistically assumes fill
        assert!(matches!(result.outcome, FillOutcome::FullFill));
        assert_eq!(result.fill_price, Some(dec!(0.49)));
    }
}
