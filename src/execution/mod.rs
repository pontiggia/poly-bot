//! Execution module - order state machine, policies, and executor
//!
//! ## Architecture
//!
//! - `state_machine`: Order state transitions and ledger invariants
//! - `policy`: Execution policies (TakerPolicy, MakerPolicy, DualPolicy)
//! - `executor`: Converts OrderIntent → orders and submits
//! - `order_tracker`: Tracks outstanding GTC orders
//! - `cancellation`: Manages order cancellation
//!
//! ## Flow
//!
//! 1. Strategy returns `OrderIntent` (WHAT it wants)
//! 2. `ExecutionPolicy` converts to `OrderParams` (HOW to execute)
//! 3. `OrderExecutor` signs and submits orders
//! 4. `OrderTracker` tracks outstanding GTC orders
//! 5. `CancellationManager` handles cleanup
//! 6. Policy handles partial fills per strategy needs

pub mod cancellation;
pub mod executor;
pub mod order_tracker;
pub mod pair_manager;
pub mod policy;
pub mod redeemer;
pub mod state_machine;

// Re-export state machine types
pub use state_machine::{
    can_cancel, check_invariants, requires_ack, validate_transition, Discrepancy,
    InvariantViolation,
};

// Re-export policy types
pub use policy::{
    DualPolicy, ExecutionPolicy, IntentRef, MakerPolicy, OrderParams, PartialFillAction,
    TakerPolicy,
};

// Re-export executor types
pub use executor::{ExecutionResult, ExecutionStatus, OrderExecutor};

// Re-export order tracker types
pub use order_tracker::{OrderTracker, TrackedOrder};

// Re-export pair manager types
pub use pair_manager::{PairManager, PairExecution, PairState, Leg, LegOutcome};

// Re-export cancellation types
pub use cancellation::CancellationManager;

// Re-export redeemer
pub use redeemer::PositionRedeemer;
