//! API authentication for Polymarket CLOB
//!
//! Generates HMAC-SHA256 signatures for authenticated API requests.
//! All authenticated requests require these headers:
//! - poly_address: Wallet address (0x + 40 hex chars)
//! - poly_api_key: API key (UUID)
//! - poly_passphrase: Passphrase (64 hex chars)
//! - poly_signature: Base64 HMAC-SHA256 signature
//! - poly_timestamp: Unix timestamp in seconds

use base64::{engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE as BASE64_URL_SAFE}, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::error::{BotError, Result};

type HmacSha256 = Hmac<Sha256>;

/// Authentication credentials for Polymarket API
#[derive(Debug, Clone)]
pub struct ApiCredentials {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
    pub wallet_address: String,
}

impl ApiCredentials {
    /// Create new credentials from configuration
    pub fn new(
        api_key: String,
        secret: String,
        passphrase: String,
        wallet_address: String,
    ) -> Self {
        Self {
            api_key,
            secret,
            passphrase,
            wallet_address,
        }
    }

    /// Generate HMAC-SHA256 signature for an API request
    ///
    /// The signature is computed as:
    /// HMAC-SHA256(secret, timestamp + method + path + body)
    ///
    /// # Arguments
    /// * `timestamp` - Unix timestamp in seconds (as string)
    /// * `method` - HTTP method (GET, POST, DELETE)
    /// * `path` - Request path (e.g., "/order")
    /// * `body` - Request body (empty string for GET/DELETE)
    ///
    /// # Returns
    /// Base64-encoded signature
    pub fn generate_signature(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String> {
        // Decode the base64-encoded secret
        // Try URL-safe first (contains - and _), then standard (contains + and /)
        let key = BASE64_URL_SAFE
            .decode(&self.secret)
            .or_else(|_| BASE64_STANDARD.decode(&self.secret))
            .map_err(|e| BotError::Signing(format!("Failed to decode secret: {}", e)))?;

        // Create HMAC instance
        let mut mac = HmacSha256::new_from_slice(&key)
            .map_err(|e| BotError::Signing(format!("Failed to create HMAC: {}", e)))?;

        // Construct the message: timestamp + method + path + body
        let message = format!("{}{}{}{}", timestamp, method, path, body);

        // Debug log the message being signed (truncate body for readability)
        let body_preview = if body.len() > 100 { &body[..100] } else { body };
        tracing::debug!(
            timestamp = %timestamp,
            method = %method,
            path = %path,
            body_preview = %body_preview,
            message_len = message.len(),
            "Signing message"
        );

        mac.update(message.as_bytes());

        // Finalize and encode as URL-safe base64 (matching Python/TS clients)
        let result = mac.finalize();
        let signature = BASE64_URL_SAFE.encode(result.into_bytes());

        Ok(signature)
    }

    /// Get current Unix timestamp in seconds as string
    pub fn current_timestamp() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs()
            .to_string()
    }
}

/// Authentication headers for Polymarket API requests
#[derive(Debug, Clone)]
pub struct AuthHeaders {
    pub poly_address: String,
    pub poly_api_key: String,
    pub poly_passphrase: String,
    pub poly_signature: String,
    pub poly_timestamp: String,
}

impl AuthHeaders {
    /// Generate authentication headers for a request
    pub fn generate(
        credentials: &ApiCredentials,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Self> {
        let timestamp = ApiCredentials::current_timestamp();
        let signature = credentials.generate_signature(&timestamp, method, path, body)?;

        Ok(Self {
            poly_address: credentials.wallet_address.clone(),
            poly_api_key: credentials.api_key.clone(),
            poly_passphrase: credentials.passphrase.clone(),
            poly_signature: signature,
            poly_timestamp: timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_generation() {
        // Test with known values (from API documentation example)
        let credentials = ApiCredentials::new(
            "test-api-key".to_string(),
            "dGVzdC1zZWNyZXQta2V5LWZvci10ZXN0aW5n".to_string(), // base64("test-secret-key-for-testing")
            "test-passphrase".to_string(),
            "0x1234567890123456789012345678901234567890".to_string(),
        );

        let result = credentials.generate_signature("1234567890", "GET", "/orders", "");
        assert!(result.is_ok());
        let signature = result.unwrap();
        assert!(!signature.is_empty());
        // Signature should be URL-safe base64 encoded
        assert!(BASE64_URL_SAFE.decode(&signature).is_ok());
    }

    #[test]
    fn test_auth_headers_generation() {
        let credentials = ApiCredentials::new(
            "test-api-key".to_string(),
            "dGVzdC1zZWNyZXQta2V5LWZvci10ZXN0aW5n".to_string(),
            "test-passphrase".to_string(),
            "0x1234567890123456789012345678901234567890".to_string(),
        );

        let headers = AuthHeaders::generate(&credentials, "POST", "/order", r#"{"test":"body"}"#);
        assert!(headers.is_ok());

        let headers = headers.unwrap();
        assert_eq!(headers.poly_api_key, "test-api-key");
        assert_eq!(
            headers.poly_address,
            "0x1234567890123456789012345678901234567890"
        );
        assert!(!headers.poly_timestamp.is_empty());
        assert!(!headers.poly_signature.is_empty());
    }

    #[test]
    fn test_timestamp_format() {
        let timestamp = ApiCredentials::current_timestamp();
        // Should be a valid number
        let parsed: u64 = timestamp.parse().expect("Timestamp should be a number");
        // Should be a reasonable Unix timestamp (after 2024)
        assert!(parsed > 1700000000);
    }

    #[test]
    fn test_url_safe_base64_secret() {
        // Test with URL-safe base64 secret (contains _ and -)
        // This is the format Polymarket uses
        let credentials = ApiCredentials::new(
            "test-api-key".to_string(),
            "ZXUszLhquwviNhvQMbleGSzAIjRPNG_L8wRcbCtyk28=".to_string(), // URL-safe base64
            "test-passphrase".to_string(),
            "0x1234567890123456789012345678901234567890".to_string(),
        );

        let result = credentials.generate_signature("1234567890", "POST", "/order", r#"{"test":"body"}"#);
        assert!(result.is_ok(), "Should handle URL-safe base64 secrets");
        let signature = result.unwrap();
        assert!(!signature.is_empty());
    }
}
