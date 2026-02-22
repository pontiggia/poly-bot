//! Legacy modules — archived arbitrage infrastructure.
//! Gated behind `#[cfg(feature = "arb")]` to keep out of default builds.
//! Preserved for reference and potential future re-enablement.

#[cfg(feature = "arb")]
pub mod arbitrage;
#[cfg(feature = "arb")]
pub mod edge_calculator;
#[cfg(feature = "arb")]
pub mod pair_manager;
