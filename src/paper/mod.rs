//! Paper trading module - simulates execution without real orders
//!
//! This module provides realistic simulation of order execution for testing
//! strategies without risking real capital.
//!
//! ## Architecture
//!
//! The paper trading system is completely separate from production execution:
//! - `FillSimulator` - Simulates whether orders would fill based on book state
//! - `PositionTracker` - Tracks simulated positions and P&L
//! - `ResolutionTracker` - Tracks market outcomes for P&L calculation
//! - `PaperAnalytics` - Aggregates statistics for analysis
//! - `PaperReport` - Generates reports (console + JSON)
//! - `PaperTrader` - Main orchestrator that coordinates all components
//!
//! ## Usage
//!
//! The bot routes to `PaperTrader` when in paper mode:
//! ```ignore
//! match config.mode {
//!     OperatingMode::Paper => paper_trader.process_intents(intents, book),
//!     OperatingMode::Live => executor.execute(intents),
//! }
//! ```

pub mod analytics;
pub mod fill_simulator;
pub mod position_tracker;
pub mod report;
pub mod resolution_tracker;
pub mod trader;

pub use analytics::{AnalyticsSummary, PaperAnalytics};
pub use fill_simulator::{ArbSimulation, FillOutcome, FillSimulator, SimulatedFill};
pub use position_tracker::{ClosedPosition, PositionSide, PositionTracker, SimulatedPosition};
pub use report::PaperReport;
pub use resolution_tracker::{MarketResolution, ResolvedOutcome, ResolutionTracker};
pub use trader::{PaperConfig, PaperStats, PaperTrader};
