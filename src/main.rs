//! Polymarket Trading Bot - Entry Point
//!
//! High-frequency trading bot for Polymarket prediction markets.

use polymarket_bot::api::MarketDiscovery;
use polymarket_bot::strategy::MarketPair;
use polymarket_bot::{Bot, Config, KillSwitch};
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("polymarket_bot=info".parse()?)
                .add_directive("info".parse()?),
        )
        .init();

    info!("===========================================");
    info!("  Polymarket Trading Bot v{}", env!("CARGO_PKG_VERSION"));
    info!("===========================================");

    // Load configuration
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load configuration: {}", e);
            error!("Make sure your .env file contains:");
            error!("  - api_key or POLYMARKET_API_KEY");
            error!("  - secret_key or POLYMARKET_SECRET");
            error!("  - passphrase or POLYMARKET_PASSPHRASE");
            error!("  - private_key, PRIVATE_KEY or POLYMARKET_PRIVATE_KEY");
            error!("  - builder_address or WALLET_ADDRESS");
            return Err(e.into());
        }
    };

    info!("Configuration loaded successfully");
    info!("  Wallet: {}", config.wallet_address);
    info!("  API Key: {}...{}", &config.api_key[..8], &config.api_key[config.api_key.len()-4..]);
    info!("  Mode: {:?}", config.mode);
    info!("  Max bet: ${}", config.max_bet_usd);
    info!("  Max daily loss: ${}", config.max_daily_loss_usd);

    if config.is_paper_mode() {
        warn!(">>> PAPER TRADING MODE - No real orders will be placed <<<");
    } else {
        warn!(">>> LIVE TRADING MODE - Real money at risk! <<<");
    }

    // Initialize kill switch
    let kill_switch = Arc::new(KillSwitch::new());
    let ks_clone = kill_switch.clone();

    // Set up Ctrl+C handler
    tokio::spawn(async move {
        if signal::ctrl_c().await.is_ok() {
            warn!("Ctrl+C received - initiating shutdown");
            ks_clone.kill();
        }
    });

    info!("Kill switch initialized");
    info!("  - Set POLYBOT_KILL=1 to stop");
    info!("  - Or create /tmp/polybot_kill");
    info!("  - Or press Ctrl+C");

    // ===========================================================================
    // MARKET DISCOVERY
    // ===========================================================================
    // Discover all crypto markets from Gamma API:
    // - 15-min markets: btc-updown-15m-{timestamp}
    // - Hourly markets: bitcoin-up-or-down-january-8-9pm-et
    // - Daily markets:  bitcoin-up-or-down-on-january-9
    // ===========================================================================

    info!("Discovering all crypto markets from Gamma API (15-min + hourly + daily)...");

    let discovery = MarketDiscovery::new();

    // Discover all crypto markets for maximum coverage
    let discovered = match discovery.discover_all_crypto().await {
        Ok(markets) => {
            // Limit to first 80 markets for broader coverage
            markets.into_iter().take(80).collect::<Vec<_>>()
        }
        Err(e) => {
            warn!("Failed to discover markets from API: {}", e);
            warn!("Falling back to hardcoded test market...");
            vec![]
        }
    };
    
    let (market_pairs, token_ids): (Vec<MarketPair>, Vec<String>) = if !discovered.is_empty() {
        info!("Discovered {} tradeable crypto markets", discovered.len());
        
        let mut pairs = Vec::new();
        let mut tokens = Vec::new();
        
        for dm in &discovered {
            info!(
                "  Market: {} ({}/{})",
                dm.condition_id, dm.first_outcome, dm.second_outcome
            );
            info!("    Question: {}", dm.question);
            info!("    First token: {}", dm.first_token_id);
            info!("    Second token: {}", dm.second_token_id);
            info!("    Fee rate: {} bps", dm.fee_rate_bps);
            
            pairs.push(dm.to_market_pair());
            tokens.push(dm.first_token_id.clone());
            tokens.push(dm.second_token_id.clone());
        }
        
        (pairs, tokens)
    } else {
        // Fallback to hardcoded market for testing when API fails
        warn!("No markets discovered - using hardcoded fallback");
        
        let yes_token = "91146426612524606788185897426983484145854573836093539884347307480474597236733".to_string();
        let no_token = "42146376778762047477642266233020835044794863565048464944940190870964136665187".to_string();
        let market_id = "0x_fallback_test".to_string();
        
        let pairs = vec![
            MarketPair::new_up_down(
                market_id,
                yes_token.clone(),
                no_token.clone(),
            )
            .with_description("FALLBACK TEST MARKET"),
        ];
        
        let tokens = vec![yes_token, no_token];
        
        (pairs, tokens)
    };

    info!("Registered {} market pair(s) with {} token(s)", market_pairs.len(), token_ids.len());

    // Create and run the bot
    let mut bot = Bot::new(config, kill_switch.clone(), token_ids, market_pairs).await;
    bot.run().await;

    // Graceful shutdown
    info!("Performing graceful shutdown...");
    polymarket_bot::kill_switch::graceful_shutdown(kill_switch).await;

    info!("Bot shutdown complete");
    Ok(())
}
