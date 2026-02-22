//! Pair Manager - lifecycle tracking for two-leg arb executions
//!
//! Tracks the state of paired arb orders (YES + NO legs) to prevent
//! orphaned one-legged positions. Each pair progresses through a state
//! machine from submission to completion or unwind.
//!
//! ## State Machine
//!
//! ```text
//! BothPending → Leg1Filled → BothFilled → Completed
//!                          → NeedUnwind → UnwindSubmitted → Completed
//! BothPending → Expired → Completed
//! BothPending → Failed (both legs rejected)
//! ```

use dashmap::DashMap;
use rust_decimal::Decimal;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// State of a two-leg arb execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairState {
    /// Both legs submitted, waiting for results
    BothPending,
    /// Leg 1 filled (fully or partially), leg 2 still pending
    Leg1Filled,
    /// Leg 2 filled (fully or partially), leg 1 still pending
    Leg2Filled,
    /// Both legs filled — success
    BothFilled,
    /// One leg filled but the other failed — need to unwind
    NeedUnwind,
    /// Unwind order has been submitted
    UnwindSubmitted,
    /// Terminal: completed (either success or after unwind)
    Completed,
    /// Terminal: both legs failed, no exposure
    Failed,
}

impl PairState {
    /// Is this a terminal state?
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

impl std::fmt::Display for PairState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BothPending => write!(f, "BothPending"),
            Self::Leg1Filled => write!(f, "Leg1Filled"),
            Self::Leg2Filled => write!(f, "Leg2Filled"),
            Self::BothFilled => write!(f, "BothFilled"),
            Self::NeedUnwind => write!(f, "NeedUnwind"),
            Self::UnwindSubmitted => write!(f, "UnwindSubmitted"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

/// Which leg of the pair
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    Leg1,
    Leg2,
}

/// Outcome of a leg execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegOutcome {
    /// Leg was filled (fully or partially)
    Filled,
    /// Leg was pending on book (GTC/partial)
    Pending,
    /// Leg was rejected or failed
    Failed,
}

/// Tracks a single two-leg arb execution
#[derive(Debug, Clone)]
pub struct PairExecution {
    /// Group ID linking the two legs
    pub group_id: String,
    /// Current state
    pub state: PairState,
    /// Leg 1 order ID (if submitted)
    pub leg1_order_id: Option<String>,
    /// Leg 1 token ID
    pub leg1_token_id: String,
    /// Leg 1 fill size
    pub leg1_fill_size: Decimal,
    /// Leg 2 order ID (if submitted)
    pub leg2_order_id: Option<String>,
    /// Leg 2 token ID
    pub leg2_token_id: String,
    /// Leg 2 fill size
    pub leg2_fill_size: Decimal,
    /// When this pair was created
    pub created_at: Instant,
}

impl PairExecution {
    /// Create a new pair execution
    pub fn new(
        group_id: String,
        leg1_token_id: String,
        leg2_token_id: String,
    ) -> Self {
        Self {
            group_id,
            state: PairState::BothPending,
            leg1_order_id: None,
            leg1_token_id,
            leg1_fill_size: Decimal::ZERO,
            leg2_order_id: None,
            leg2_token_id,
            leg2_fill_size: Decimal::ZERO,
            created_at: Instant::now(),
        }
    }

    /// How long this pair has been active
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }
}

/// Manages lifecycle of paired arb executions
pub struct PairManager {
    /// Active pair executions by group_id
    pairs: DashMap<String, PairExecution>,
    /// Total pairs ever tracked
    total_created: std::sync::atomic::AtomicU64,
    /// Total pairs completed successfully (both legs filled)
    total_success: std::sync::atomic::AtomicU64,
    /// Total pairs that required unwind
    total_unwound: std::sync::atomic::AtomicU64,
    /// Total pairs that failed (no exposure)
    total_failed: std::sync::atomic::AtomicU64,
}

impl PairManager {
    /// Create a new pair manager
    pub fn new() -> Self {
        Self {
            pairs: DashMap::new(),
            total_created: std::sync::atomic::AtomicU64::new(0),
            total_success: std::sync::atomic::AtomicU64::new(0),
            total_unwound: std::sync::atomic::AtomicU64::new(0),
            total_failed: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Register a new arb pair for tracking
    pub fn begin_pair(
        &self,
        group_id: String,
        leg1_token_id: String,
        leg2_token_id: String,
    ) {
        debug!(group_id = %group_id, "Tracking new arb pair");
        let pair = PairExecution::new(group_id.clone(), leg1_token_id, leg2_token_id);
        self.pairs.insert(group_id, pair);
        self.total_created
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record the result of a leg execution
    ///
    /// Returns the new state of the pair
    pub fn on_leg_result(
        &self,
        group_id: &str,
        leg: Leg,
        order_id: Option<String>,
        outcome: LegOutcome,
        fill_size: Decimal,
    ) -> Option<PairState> {
        let mut pair = self.pairs.get_mut(group_id)?;

        // Record order ID and fill size
        match leg {
            Leg::Leg1 => {
                pair.leg1_order_id = order_id;
                pair.leg1_fill_size = fill_size;
            }
            Leg::Leg2 => {
                pair.leg2_order_id = order_id;
                pair.leg2_fill_size = fill_size;
            }
        }

        // Transition state machine
        let new_state = match (pair.state, leg, outcome) {
            // From BothPending: first leg result
            (PairState::BothPending, Leg::Leg1, LegOutcome::Filled) => PairState::Leg1Filled,
            (PairState::BothPending, Leg::Leg2, LegOutcome::Filled) => PairState::Leg2Filled,
            (PairState::BothPending, Leg::Leg1, LegOutcome::Pending) => PairState::Leg1Filled,
            (PairState::BothPending, Leg::Leg2, LegOutcome::Pending) => PairState::Leg2Filled,
            (PairState::BothPending, _, LegOutcome::Failed)
                if pair.leg1_order_id.is_none() && pair.leg2_order_id.is_none() =>
            {
                // Both legs failed (no orders placed) — no exposure
                PairState::Failed
            }
            (PairState::BothPending, _, LegOutcome::Failed) => {
                // First leg failed — stay pending, wait for second leg result
                PairState::BothPending
            }

            // From Leg1Filled: second leg result
            (PairState::Leg1Filled, Leg::Leg2, LegOutcome::Filled) => PairState::BothFilled,
            (PairState::Leg1Filled, Leg::Leg2, LegOutcome::Pending) => PairState::BothFilled,
            (PairState::Leg1Filled, Leg::Leg2, LegOutcome::Failed) => PairState::NeedUnwind,

            // From Leg2Filled: first leg result
            (PairState::Leg2Filled, Leg::Leg1, LegOutcome::Filled) => PairState::BothFilled,
            (PairState::Leg2Filled, Leg::Leg1, LegOutcome::Pending) => PairState::BothFilled,
            (PairState::Leg2Filled, Leg::Leg1, LegOutcome::Failed) => PairState::NeedUnwind,

            // Already terminal or unexpected transition
            (state, _, _) => {
                warn!(
                    group_id = %group_id,
                    current_state = %state,
                    leg = ?leg,
                    outcome = ?outcome,
                    "Unexpected pair state transition"
                );
                state
            }
        };

        pair.state = new_state;

        debug!(
            group_id = %group_id,
            state = %new_state,
            leg1_fill = %pair.leg1_fill_size,
            leg2_fill = %pair.leg2_fill_size,
            "Pair state transition"
        );

        Some(new_state)
    }

    /// Get all pairs that need unwinding (one leg filled, other failed)
    pub fn needs_unwind(&self) -> Vec<String> {
        self.pairs
            .iter()
            .filter(|entry| entry.state == PairState::NeedUnwind)
            .map(|entry| entry.group_id.clone())
            .collect()
    }

    /// Get all pairs stuck beyond max_age
    pub fn stale_pairs(&self, max_age: Duration) -> Vec<String> {
        self.pairs
            .iter()
            .filter(|entry| !entry.state.is_terminal() && entry.age() > max_age)
            .map(|entry| entry.group_id.clone())
            .collect()
    }

    /// Mark a pair as having started unwind
    pub fn mark_unwinding(&self, group_id: &str) {
        if let Some(mut pair) = self.pairs.get_mut(group_id) {
            pair.state = PairState::UnwindSubmitted;
        }
    }

    /// Mark a pair as completed and record stats
    pub fn complete(&self, group_id: &str, success: bool) {
        if let Some(mut pair) = self.pairs.get_mut(group_id) {
            pair.state = PairState::Completed;

            if success {
                self.total_success
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                self.total_unwound
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Remove from active tracking
        self.pairs.remove(group_id);
    }

    /// Mark a pair as failed (no exposure)
    pub fn mark_failed(&self, group_id: &str) {
        if let Some(mut pair) = self.pairs.get_mut(group_id) {
            pair.state = PairState::Failed;
        }
        self.total_failed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pairs.remove(group_id);
    }

    /// Get a pair by group ID
    pub fn get(&self, group_id: &str) -> Option<PairExecution> {
        self.pairs.get(group_id).map(|e| e.clone())
    }

    /// Number of active (non-terminal) pairs
    pub fn active_count(&self) -> usize {
        self.pairs.len()
    }

    /// Log current stats
    pub fn log_stats(&self) {
        use std::sync::atomic::Ordering;
        info!(
            active = self.active_count(),
            total = self.total_created.load(Ordering::Relaxed),
            success = self.total_success.load(Ordering::Relaxed),
            unwound = self.total_unwound.load(Ordering::Relaxed),
            failed = self.total_failed.load(Ordering::Relaxed),
            "PairManager stats"
        );
    }
}

impl Default for PairManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_both_filled_success() {
        let pm = PairManager::new();
        pm.begin_pair("arb-1".into(), "yes_token".into(), "no_token".into());

        // Leg 1 fills
        let state = pm
            .on_leg_result("arb-1", Leg::Leg1, Some("order-1".into()), LegOutcome::Filled, dec!(10))
            .unwrap();
        assert_eq!(state, PairState::Leg1Filled);

        // Leg 2 fills
        let state = pm
            .on_leg_result("arb-1", Leg::Leg2, Some("order-2".into()), LegOutcome::Filled, dec!(10))
            .unwrap();
        assert_eq!(state, PairState::BothFilled);

        pm.complete("arb-1", true);
        assert_eq!(pm.active_count(), 0);
    }

    #[test]
    fn test_leg2_fails_needs_unwind() {
        let pm = PairManager::new();
        pm.begin_pair("arb-2".into(), "yes_token".into(), "no_token".into());

        // Leg 1 fills
        let state = pm
            .on_leg_result("arb-2", Leg::Leg1, Some("order-1".into()), LegOutcome::Filled, dec!(10))
            .unwrap();
        assert_eq!(state, PairState::Leg1Filled);

        // Leg 2 fails
        let state = pm
            .on_leg_result("arb-2", Leg::Leg2, None, LegOutcome::Failed, Decimal::ZERO)
            .unwrap();
        assert_eq!(state, PairState::NeedUnwind);

        // Check needs_unwind
        let unwind = pm.needs_unwind();
        assert_eq!(unwind.len(), 1);
        assert_eq!(unwind[0], "arb-2");

        // Mark unwinding
        pm.mark_unwinding("arb-2");
        assert_eq!(pm.get("arb-2").unwrap().state, PairState::UnwindSubmitted);

        // Complete unwind
        pm.complete("arb-2", false);
        assert_eq!(pm.active_count(), 0);
    }

    #[test]
    fn test_stale_pair_detection() {
        let pm = PairManager::new();
        pm.begin_pair("arb-3".into(), "yes_token".into(), "no_token".into());

        // Fresh pair is not stale
        assert!(pm.stale_pairs(Duration::from_secs(60)).is_empty());

        // Manually check that a pair with old timestamp would be detected
        // (Can't easily test time-based logic without mocking, but the logic is straightforward)
    }

    #[test]
    fn test_mark_failed() {
        let pm = PairManager::new();
        pm.begin_pair("arb-4".into(), "yes_token".into(), "no_token".into());

        pm.mark_failed("arb-4");
        assert_eq!(pm.active_count(), 0);
    }
}
