use std::env;
use std::str::FromStr as _;

use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use alloy::sol;
use alloy::primitives::{U256, FixedBytes};
use polymarket_client_sdk::types::{Address, address};
use polymarket_client_sdk::{POLYGON, contract_config};

const RPC_URL: &str = "https://polygon-rpc.com";
const USDC: Address = address!("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"); // USDC on Polygon

sol! {
    #[sol(rpc)]
    interface IConditionalTokens {
        function redeemPositions(address collateralToken, bytes32 parentCollectionId, bytes32 conditionId, uint256[] indexSets) external;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let private_key = env::var("POLYMARKET_PRIVATE_KEY").expect("Need POLYMARKET_PRIVATE_KEY");
    let signer = LocalSigner::from_str(&private_key)?.with_chain_id(Some(POLYGON));

    let provider = ProviderBuilder::new().wallet(signer.clone()).connect(RPC_URL).await?;

    // Get the configured Conditional Tokens contract address for the chain
    let chain = POLYGON;
    let cfg = contract_config(chain, false).expect("contract_config available");
    let ctf_addr = cfg.conditional_tokens;
    let ctf = IConditionalTokens::new(ctf_addr, provider.clone());

    // parentCollectionId is null in Polymarket case
    let parent_collection: [u8;32] = [0u8; 32];

    // condition_id: will be read from the environment variable `CONDITION_ID` if set.
    // You can find the condition id using the included `examples/find_condition.rs` helper
    // or via the Gamma API. CONDITION_ID must be a 32-byte hex string (0x...)
    let condition_hex = std::env::var("CONDITION_ID").expect("Need CONDITION_ID env var (32-byte hex)");
    let condition_id: [u8;32] = hex::decode(condition_hex.trim_start_matches("0x"))?.try_into().expect("CONDITION_ID must be 32 bytes");

    // indexSets: for binary markets use the winning outcome index as a bitmask
    // - YES / first outcome  => 0b01 = 1
    // - NO  / second outcome => 0b10 = 2
    // Provide WINNING_INDEX env var with value `1` or `2` (default = 1)
    let winning_index: u64 = std::env::var("WINNING_INDEX").ok().and_then(|s| s.parse().ok()).unwrap_or(1u64);
    let index_sets: Vec<U256> = vec![U256::from(winning_index)];

    // convert arrays into FixedBytes<32> expected by the generated bindings
    let parent_collection_fb: FixedBytes<32> = FixedBytes::from(parent_collection);
    let condition_fb: FixedBytes<32> = FixedBytes::from(condition_id);

    let tx_hash = ctf
    
        .redeemPositions(
            USDC,
            parent_collection_fb,
            condition_fb,
            index_sets,
        )
        .send()
        .await?
        .watch()
        .await?;

    println!("Redeem tx: {:?}", tx_hash);
    Ok(())
}