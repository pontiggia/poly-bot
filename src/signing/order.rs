//! EIP-712 order signing for Polymarket CTF Exchange
//!
//! Implements order signing according to the Polymarket CTF Exchange specification.
//! Uses alloy for Ethereum primitives and signing.
//!
//! ## Order Structure
//!
//! The order struct for EIP-712 signing:
//! - salt: u64 - Random nonce for uniqueness
//! - maker: Address - Proxy wallet (trading address)
//! - signer: Address - EOA signer (API key address)
//! - taker: Address - 0x0...0 for any taker
//! - tokenId: u256 - Asset ID
//! - makerAmount: u256 - Amount maker provides (USDC, 6 decimals)
//! - takerAmount: u256 - Amount maker receives (tokens, 6 decimals)
//! - expiration: u256 - Unix timestamp or 0 for no expiry
//! - nonce: u256 - Order nonce (usually 0)
//! - feeRateBps: u256 - Fee in basis points
//! - side: u8 - 0=BUY, 1=SELL
//! - signatureType: u8 - 1=EOA, 2=Gnosis Safe

use alloy::primitives::{keccak256, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::api::types::{Side, SignedOrder};
use crate::constants::{CHAIN_ID, EXCHANGE_CONTRACT, NEG_RISK_EXCHANGE_CONTRACT};
use crate::error::{BotError, Result};

/// EIP-712 domain separator type hash
/// keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
/// Standard EIP-712 domain type hash used by OpenZeppelin's EIP712 implementation
const DOMAIN_TYPE_HASH: &str = "8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f";

/// Order type hash for Polymarket CTF Exchange
/// keccak256("Order(uint256 salt,address maker,address signer,address taker,uint256 tokenId,uint256 makerAmount,uint256 takerAmount,uint256 expiration,uint256 nonce,uint256 feeRateBps,uint8 side,uint8 signatureType)")
/// Source: https://github.com/Polymarket/ctf-exchange/blob/main/src/exchange/libraries/OrderStructs.sol
const ORDER_TYPE_HASH: &str = "a852566c4e14d00869b6db0220888a9090a13eccdaea03713ff0a3d27bf9767c";

/// Order data for signing
#[derive(Debug, Clone)]
pub struct Order {
    pub salt: u64,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    pub token_id: String,
    pub maker_amount: String,
    pub taker_amount: String,
    pub expiration: String,
    pub nonce: String,
    pub fee_rate_bps: String,
    pub side: Side,
    pub signature_type: u8,
}

impl Order {
    /// Create a new order with default values
    pub fn new(
        maker: String,
        signer: String,
        token_id: String,
        side: Side,
        price: Decimal,
        size: Decimal,
    ) -> Self {
        // Generate random salt
        let salt = rand_salt();

        // Calculate amounts based on side and price
        // For BUY: makerAmount = USDC to spend, takerAmount = tokens to receive
        // For SELL: makerAmount = tokens to sell, takerAmount = USDC to receive
        let (maker_amount, taker_amount) = calculate_amounts(side, price, size);

        Self {
            salt,
            maker,
            signer,
            taker: "0x0000000000000000000000000000000000000000".to_string(),
            token_id,
            maker_amount,
            taker_amount,
            expiration: "0".to_string(), // No expiry
            nonce: "0".to_string(),
            fee_rate_bps: "0".to_string(),
            side,
            signature_type: 1, // EOA signature
        }
    }

    /// Convert to SignedOrder (without signature)
    pub fn to_unsigned_order(&self) -> SignedOrder {
        SignedOrder {
            salt: self.salt,
            maker: self.maker.clone(),
            signer: self.signer.clone(),
            taker: self.taker.clone(),
            token_id: self.token_id.clone(),
            maker_amount: self.maker_amount.clone(),
            taker_amount: self.taker_amount.clone(),
            side: self.side,
            expiration: self.expiration.clone(),
            nonce: self.nonce.clone(),
            fee_rate_bps: self.fee_rate_bps.clone(),
            signature_type: self.signature_type,
            signature: String::new(),
        }
    }

    /// Calculate the EIP-712 struct hash for this order
    pub fn struct_hash(&self) -> Result<B256> {
        // Parse order type hash
        let order_type_hash = B256::from_str(&format!("0x{}", ORDER_TYPE_HASH))
            .map_err(|e| BotError::Signing(format!("Invalid order type hash: {}", e)))?;

        // Parse and encode all order fields
        let salt = U256::from(self.salt);
        let maker = parse_address(&self.maker)?;
        let signer_addr = parse_address(&self.signer)?;
        let taker = parse_address(&self.taker)?;
        let token_id = parse_u256(&self.token_id)?;
        let maker_amount = parse_u256(&self.maker_amount)?;
        let taker_amount = parse_u256(&self.taker_amount)?;
        let expiration = parse_u256(&self.expiration)?;
        let nonce = parse_u256(&self.nonce)?;
        let fee_rate_bps = parse_u256(&self.fee_rate_bps)?;
        let side = U256::from(side_to_u8(self.side));
        let signature_type = U256::from(self.signature_type);

        // Debug log all parsed values
        tracing::debug!(
            salt = %salt,
            maker = %self.maker,
            signer = %self.signer,
            token_id = %token_id,
            maker_amount = %maker_amount,
            taker_amount = %taker_amount,
            side = %side,
            signature_type = %signature_type,
            "Struct hash input values"
        );

        // Encode the struct: typeHash || fields
        let mut encoded = Vec::with_capacity(32 * 13);
        encoded.extend_from_slice(order_type_hash.as_slice());
        encoded.extend_from_slice(&salt.to_be_bytes::<32>());
        encoded.extend_from_slice(&[0u8; 12]); // padding for address
        encoded.extend_from_slice(maker.as_slice());
        encoded.extend_from_slice(&[0u8; 12]);
        encoded.extend_from_slice(signer_addr.as_slice());
        encoded.extend_from_slice(&[0u8; 12]);
        encoded.extend_from_slice(taker.as_slice());
        encoded.extend_from_slice(&token_id.to_be_bytes::<32>());
        encoded.extend_from_slice(&maker_amount.to_be_bytes::<32>());
        encoded.extend_from_slice(&taker_amount.to_be_bytes::<32>());
        encoded.extend_from_slice(&expiration.to_be_bytes::<32>());
        encoded.extend_from_slice(&nonce.to_be_bytes::<32>());
        encoded.extend_from_slice(&fee_rate_bps.to_be_bytes::<32>());
        encoded.extend_from_slice(&side.to_be_bytes::<32>());
        encoded.extend_from_slice(&signature_type.to_be_bytes::<32>());

        let result = keccak256(&encoded);
        tracing::debug!(
            encoded_len = encoded.len(),
            struct_hash = %hex::encode(result.as_slice()),
            "Computed struct hash"
        );

        Ok(result)
    }
}

/// Order builder for fluent construction
pub struct OrderBuilder {
    order: Order,
}

impl OrderBuilder {
    pub fn new(maker: String, signer: String, token_id: String, side: Side) -> Self {
        Self {
            order: Order {
                salt: rand_salt(),
                maker,
                signer,
                taker: "0x0000000000000000000000000000000000000000".to_string(),
                token_id,
                maker_amount: "0".to_string(),
                taker_amount: "0".to_string(),
                expiration: "0".to_string(),
                nonce: "0".to_string(),
                fee_rate_bps: "0".to_string(),
                side,
                signature_type: 1,
            },
        }
    }

    pub fn with_amounts(mut self, maker_amount: String, taker_amount: String) -> Self {
        self.order.maker_amount = maker_amount;
        self.order.taker_amount = taker_amount;
        self
    }

    pub fn with_price_size(mut self, price: Decimal, size: Decimal) -> Self {
        let (maker_amount, taker_amount) = calculate_amounts(self.order.side, price, size);
        self.order.maker_amount = maker_amount;
        self.order.taker_amount = taker_amount;
        self
    }

    pub fn with_salt(mut self, salt: u64) -> Self {
        self.order.salt = salt;
        self
    }

    pub fn with_expiration(mut self, expiration: u64) -> Self {
        self.order.expiration = expiration.to_string();
        self
    }

    pub fn with_nonce(mut self, nonce: u64) -> Self {
        self.order.nonce = nonce.to_string();
        self
    }

    pub fn with_fee_rate_bps(mut self, fee_rate_bps: u32) -> Self {
        self.order.fee_rate_bps = fee_rate_bps.to_string();
        self
    }

    pub fn with_taker(mut self, taker: String) -> Self {
        self.order.taker = taker;
        self
    }

    pub fn build(self) -> Order {
        self.order
    }
}

/// Order signer using private key
pub struct OrderSigner {
    signer: PrivateKeySigner,
    domain_separator: B256,
    neg_risk_domain_separator: B256,
}

impl OrderSigner {
    /// Create a new order signer from a private key
    ///
    /// # Arguments
    /// * `private_key` - Hex-encoded private key (with or without 0x prefix)
    pub fn new(private_key: &str) -> Result<Self> {
        // Parse private key
        let key = private_key.trim_start_matches("0x");
        let signer: PrivateKeySigner = key
            .parse()
            .map_err(|e| BotError::Signing(format!("Invalid private key: {}", e)))?;

        // Calculate domain separators
        let domain_separator = calculate_domain_separator(EXCHANGE_CONTRACT)?;
        let neg_risk_domain_separator = calculate_domain_separator(NEG_RISK_EXCHANGE_CONTRACT)?;

        Ok(Self {
            signer,
            domain_separator,
            neg_risk_domain_separator,
        })
    }

    /// Get the signer's address (EIP-55 checksummed format)
    pub fn address(&self) -> String {
        // Use checksummed format - Polymarket API expects EIP-55 checksummed addresses
        self.signer.address().to_checksum(None)
    }

    /// Sign an order for the standard CTF Exchange
    pub async fn sign_order(&self, order: &Order) -> Result<SignedOrder> {
        self.sign_order_with_domain(order, &self.domain_separator)
            .await
    }

    /// Sign an order for the Neg Risk CTF Exchange
    pub async fn sign_order_neg_risk(&self, order: &Order) -> Result<SignedOrder> {
        self.sign_order_with_domain(order, &self.neg_risk_domain_separator)
            .await
    }

    /// Sign an order with a specific domain separator
    async fn sign_order_with_domain(
        &self,
        order: &Order,
        domain_separator: &B256,
    ) -> Result<SignedOrder> {
        // Calculate struct hash
        let struct_hash = order.struct_hash()?;

        // Calculate EIP-712 signing hash: keccak256("\x19\x01" || domainSeparator || structHash)
        let mut digest_input = Vec::with_capacity(66);
        digest_input.extend_from_slice(&[0x19, 0x01]);
        digest_input.extend_from_slice(domain_separator.as_slice());
        digest_input.extend_from_slice(struct_hash.as_slice());
        let digest = keccak256(&digest_input);

        // Debug: Log signing details
        tracing::debug!(
            domain_separator = %hex::encode(domain_separator.as_slice()),
            struct_hash = %hex::encode(struct_hash.as_slice()),
            digest = %hex::encode(digest.as_slice()),
            signer_address = %self.address(),
            "EIP-712 signing details"
        );

        // Sign the digest
        let signature = self
            .signer
            .sign_hash(&digest)
            .await
            .map_err(|e| BotError::Signing(format!("Failed to sign order: {}", e)))?;

        // Format signature as 0x + r + s + v (65 bytes total)
        let sig_bytes = signature.as_bytes();
        let signature_hex = format!("0x{}", hex::encode(sig_bytes));

        tracing::debug!(
            signature_len = sig_bytes.len(),
            signature_hex = %signature_hex,
            "Generated signature"
        );

        // Create signed order
        let mut signed_order = order.to_unsigned_order();
        signed_order.signature = signature_hex;

        Ok(signed_order)
    }
}

/// Calculate domain separator for EIP-712
fn calculate_domain_separator(verifying_contract: &str) -> Result<B256> {
    // Domain type hash
    let domain_type_hash = B256::from_str(&format!("0x{}", DOMAIN_TYPE_HASH))
        .map_err(|e| BotError::Signing(format!("Invalid domain type hash: {}", e)))?;

    // Name hash: keccak256("Polymarket CTF Exchange")
    let name_hash = keccak256(b"Polymarket CTF Exchange");

    // Version hash: keccak256("1")
    let version_hash = keccak256(b"1");

    // Chain ID
    let chain_id = U256::from(CHAIN_ID);

    // Verifying contract
    let contract = parse_address(verifying_contract)?;

    // Encode: typeHash || nameHash || versionHash || chainId || verifyingContract
    let mut encoded = Vec::with_capacity(32 * 5);
    encoded.extend_from_slice(domain_type_hash.as_slice());
    encoded.extend_from_slice(name_hash.as_slice());
    encoded.extend_from_slice(version_hash.as_slice());
    encoded.extend_from_slice(&chain_id.to_be_bytes::<32>());
    encoded.extend_from_slice(&[0u8; 12]); // padding for address
    encoded.extend_from_slice(contract.as_slice());

    let result = keccak256(&encoded);
    tracing::info!(
        contract = %verifying_contract,
        domain_separator = %hex::encode(result.as_slice()),
        name_hash = %hex::encode(name_hash.as_slice()),
        version_hash = %hex::encode(version_hash.as_slice()),
        "Computed domain separator"
    );

    Ok(result)
}

/// Parse an Ethereum address from string
fn parse_address(s: &str) -> Result<Address> {
    Address::from_str(s).map_err(|e| BotError::Signing(format!("Invalid address '{}': {}", s, e)))
}

/// Convert an address string to EIP-55 checksummed format
pub fn to_checksum_address(addr: &str) -> Result<String> {
    let address = parse_address(addr)?;
    Ok(address.to_checksum(None))
}

/// Parse a U256 from string
fn parse_u256(s: &str) -> Result<U256> {
    U256::from_str(s).map_err(|e| BotError::Signing(format!("Invalid U256 '{}': {}", s, e)))
}

/// Convert Side to u8 (0=BUY, 1=SELL)
fn side_to_u8(side: Side) -> u8 {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

/// Generate a random salt for order uniqueness
fn rand_salt() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_nanos() as u64;
    // Mix with a simple hash to add more entropy
    timestamp ^ (timestamp >> 17) ^ (timestamp << 7)
}

/// Calculate maker and taker amounts based on side, price, and size
///
/// Amounts are in base units (6 decimals for USDC/tokens)
/// - BUY: makerAmount = USDC to spend, takerAmount = tokens to receive
/// - SELL: makerAmount = tokens to sell, takerAmount = USDC to receive
///
/// Polymarket API requires both amounts to have max 2 decimal places
/// (must be divisible by 10000 in base units)
fn calculate_amounts(side: Side, price: Decimal, size: Decimal) -> (String, String) {
    let scale = Decimal::from(1_000_000u64); // 6 decimals
    let round_to = Decimal::from(10_000u64); // 2 decimal places

    match side {
        Side::Buy => {
            // BUY: spend USDC, receive tokens - both 2 decimal places
            let maker_raw = (size * price * scale).trunc();
            let taker_raw = (size * scale).trunc();

            let maker_amount = (maker_raw / round_to).trunc() * round_to;
            let taker_amount = (taker_raw / round_to).trunc() * round_to;

            (maker_amount.to_string(), taker_amount.to_string())
        }
        Side::Sell => {
            // SELL: sell tokens, receive USDC - both 2 decimal places
            let maker_raw = (size * scale).trunc();
            let taker_raw = (size * price * scale).trunc();

            let maker_amount = (maker_raw / round_to).trunc() * round_to;
            let taker_amount = (taker_raw / round_to).trunc() * round_to;

            (maker_amount.to_string(), taker_amount.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_order_creation() {
        let order = Order::new(
            "0x1234567890123456789012345678901234567890".to_string(),
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
            "12345678901234567890".to_string(),
            Side::Buy,
            dec!(0.50),
            dec!(10.0),
        );

        assert_eq!(order.side, Side::Buy);
        assert!(!order.maker_amount.is_empty());
        assert!(!order.taker_amount.is_empty());
    }

    #[test]
    fn test_calculate_amounts_buy() {
        // BUY 10 tokens at $0.50 each
        let (maker, taker) = calculate_amounts(Side::Buy, dec!(0.50), dec!(10.0));
        // makerAmount = 10 * 0.5 * 1_000_000 = 5_000_000
        // takerAmount = 10 * 1_000_000 = 10_000_000
        assert_eq!(maker, "5000000");
        assert_eq!(taker, "10000000");
    }

    #[test]
    fn test_calculate_amounts_sell() {
        // SELL 10 tokens at $0.50 each
        let (maker, taker) = calculate_amounts(Side::Sell, dec!(0.50), dec!(10.0));
        // makerAmount = 10 * 1_000_000 = 10_000_000
        // takerAmount = 10 * 0.5 * 1_000_000 = 5_000_000
        assert_eq!(maker, "10000000");
        assert_eq!(taker, "5000000");
    }

    #[test]
    fn test_order_builder() {
        let order = OrderBuilder::new(
            "0x1234567890123456789012345678901234567890".to_string(),
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
            "12345678901234567890".to_string(),
            Side::Buy,
        )
        .with_price_size(dec!(0.31), dec!(3.2258))
        .with_fee_rate_bps(0)
        .build();

        assert_eq!(order.fee_rate_bps, "0");
        assert_eq!(order.side, Side::Buy);
    }

    #[test]
    fn test_side_to_u8() {
        assert_eq!(side_to_u8(Side::Buy), 0);
        assert_eq!(side_to_u8(Side::Sell), 1);
    }

    #[test]
    fn test_rand_salt() {
        let salt1 = rand_salt();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let salt2 = rand_salt();
        // Salts should be different
        assert_ne!(salt1, salt2);
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("0x1234567890123456789012345678901234567890");
        assert!(addr.is_ok());

        let invalid = parse_address("not-an-address");
        assert!(invalid.is_err());
    }

    #[test]
    fn test_parse_u256() {
        let num = parse_u256("12345678901234567890");
        assert!(num.is_ok());

        let zero = parse_u256("0");
        assert!(zero.is_ok());
    }
}
