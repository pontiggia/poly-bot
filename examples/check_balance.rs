use std::env;
use std::str::FromStr as _;

use alloy::providers::ProviderBuilder;
use alloy::signers::local::LocalSigner;
use alloy::signers::Signer as _;
use alloy::sol;
use alloy::primitives::U256;
use polymarket_client_sdk::{POLYGON, contract_config};

const RPC_URL: &str = "https://polygon-rpc.com";

sol!{
    #[sol(rpc)]
    interface IERC1155 {
        function balanceOf(address account, uint256 id) external view returns (uint256);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = env::args().collect();
    let token_id_str = args.get(1)
        .map(|s| s.to_owned())
        .or_else(|| env::var("TOKEN_ID").ok())
        .expect("Provide TOKEN_ID as first arg or TOKEN_ID env var");

    // Use private key from env if present so we can default owner to that EOA
    let private_key = env::var("POLYMARKET_PRIVATE_KEY").ok();

    let provider = if let Some(pk) = private_key.clone() {
        let signer = LocalSigner::from_str(&pk)?; // no chain id needed for view calls
        ProviderBuilder::new().wallet(signer.clone()).connect(RPC_URL).await?
    } else {
        ProviderBuilder::new().connect(RPC_URL).await?
    };

    let chain = POLYGON;
    let cfg = contract_config(chain, false)?;
    let ctf_addr = cfg.conditional_tokens;
    let ctf = IERC1155::new(ctf_addr, provider.clone());

    // owner address: use OWNER env var if set, otherwise use signer address from env private key
    let owner: alloy::types::Address = if let Some(owner_str) = env::var("OWNER_ADDRESS").ok() {
        owner_str.parse()?
    } else if let Some(pk) = private_key {
        let signer = LocalSigner::from_str(&pk)?;
        signer.address()
    } else {
        panic!("Provide OWNER_ADDRESS env var or POLYMARKET_PRIVATE_KEY to infer owner");
    };

    let token_u256 = U256::from_dec_str(&token_id_str)?;

    let balance = ctf.balanceOf(owner, token_u256).call().await?;

    println!("Owner: {}", owner);
    println!("Token id: {}", token_id_str);
    println!("Balance: {}", balance);

    Ok(())
}
