use std::env;
use std::str::FromStr as _;

use alloy::primitives::U256;
use alloy::providers::ProviderBuilder;
use alloy::signers::local::LocalSigner;
use alloy::sol;
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
    let private_key = env::var("POLYMARKET_PRIVATE_KEY")
        .expect("Need POLYMARKET_PRIVATE_KEY env var");
    let signer = LocalSigner::from_str(&private_key)?;
    let owner = signer.address();

    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(RPC_URL)
        .await?;

    let chain = POLYGON;
    let cfg = contract_config(chain, false).expect("contract_config for Polygon");
    let ctf_addr = cfg.conditional_tokens;
    let ctf = IERC1155::new(ctf_addr, provider.clone());

    let token_u256 = U256::from_str_radix(&token_id_str, 10)
        .expect("Invalid token ID — must be a decimal integer");

    let balance = ctf.balanceOf(owner, token_u256).call().await?;

    println!("Owner: {}", owner);
    println!("Token id: {}", token_id_str);
    println!("Balance: {}", balance);

    Ok(())
}
