//! EIP-712 signing module for Polymarket orders
//!
//! Provides cryptographic signing for Polymarket CTF Exchange orders
//! using the EIP-712 typed data standard.

pub mod order;

pub use order::{to_checksum_address, Order, OrderBuilder, OrderSigner};
