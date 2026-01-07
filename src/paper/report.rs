//! Paper report - generates human-readable and machine-parseable reports
//!
//! Produces both console output and JSON for analysis.

use crate::paper::analytics::AnalyticsSummary;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;

// ============================================================================
// PAPER REPORT
// ============================================================================

/// Generates reports from analytics summary
pub struct PaperReport {
    summary: AnalyticsSummary,
}

impl PaperReport {
    /// Create a new report from analytics summary
    pub fn new(summary: AnalyticsSummary) -> Self {
        Self { summary }
    }

    /// Generate console-friendly report with box drawing
    pub fn to_console(&self) -> String {
        let s = &self.summary;
        let duration = format_duration(s.session_duration);

        let verdict_taker = if s.taker_profitable {
            "PROFITABLE ✅"
        } else {
            "NOT PROFITABLE ❌"
        };
        let verdict_maker = if s.maker_profitable {
            "PROFITABLE ✅"
        } else {
            "NOT PROFITABLE ❌"
        };

        let recommendation = if s.maker_profitable && !s.taker_profitable {
            "Use MAKER mode only"
        } else if s.taker_profitable && s.maker_profitable {
            "Both modes profitable - MAKER preferred (higher profit)"
        } else if s.taker_profitable {
            "TAKER mode profitable"
        } else {
            "Neither mode profitable - review strategy"
        };

        // Warning for partial arbs
        let partial_warning = if s.partial_arb_rate > 0.05 {
            format!(
                "\n║ ⚠️  WARNING: High partial arb rate ({:.1}%) - risk of imbalanced positions",
                s.partial_arb_rate * 100.0
            )
        } else {
            String::new()
        };

        format!(
            r#"
╔════════════════════════════════════════════════════════════════════╗
║            PAPER TRADING REPORT - {duration:^20}            ║
╠════════════════════════════════════════════════════════════════════╣
║ MARKET ACTIVITY                                                    ║
║ ───────────────                                                    ║
║ Session Duration:     {duration:<20}                          ║
║ Book Updates:         {:<20} ({:.1}/sec)               ║
╠════════════════════════════════════════════════════════════════════╣
║ STRATEGY PERFORMANCE                                               ║
║ ────────────────────                                               ║
║ Opportunities Found:  {:<20} ({:.2}/hour)              ║
║ Order Intents:        {:<20}                                 ║
║                                                                    ║
║ Fill Simulation:                                                   ║
║   Would Full Fill:    {:<8} ({:>5.1}%)                             ║
║   Would Partial Fill: {:<8} ({:>5.1}%)                             ║
║   Would Not Fill:     {:<8} ({:>5.1}%)                             ║
║   Fill Rate:          {:.1}%                                       ║
╠════════════════════════════════════════════════════════════════════╣
║ ARB EXECUTION                                                      ║
║ ─────────────                                                      ║
║ Arb Attempts:         {:<20}                                 ║
║   Both Legs Fill:     {:<8} ({:>5.1}%) ← Target               ║
║   One Leg Only:       {:<8} ({:>5.1}%) ⚠️ Risk                ║
║   Neither Leg:        {:<8} ({:>5.1}%)                             ║
║ Arb Success Rate:     {:.1}%                                       ║{partial_warning}
╠════════════════════════════════════════════════════════════════════╣
║ P&L SIMULATION                                                     ║
║ ──────────────                                                     ║
║ Gross Edge Captured:  ${:<19}                                ║
║                                                                    ║
║ TAKER MODE (3% fee):                                               ║
║   Fees Paid:          ${:<19}                                ║
║   Net P&L:            ${:<19} {:<14}             ║
║   Projected Daily:    ${:<19}                                ║
║                                                                    ║
║ MAKER MODE (0% fee):                                               ║
║   Fees Paid:          ${:<19}                                ║
║   Net P&L:            ${:<19} {:<14}             ║
║   Projected Daily:    ${:<19}                                ║
╠════════════════════════════════════════════════════════════════════╣
║ EDGE ANALYSIS                                                      ║
║ ─────────────                                                      ║
║ Average Edge:         {:.2}%                                       ║
║ Min Edge:             {:.2}%                                       ║
║ Max Edge:             {:.2}%                                       ║
║ Average Slippage:     {:.2} cents                                  ║
║ Max Slippage:         {:.2} cents                                  ║
╠════════════════════════════════════════════════════════════════════╣
║ VERDICT                                                            ║
║ ───────                                                            ║
║ Taker Mode: {:<54}║
║ Maker Mode: {:<54}║
║                                                                    ║
║ Recommendation: {:<50}║
╚════════════════════════════════════════════════════════════════════╝
"#,
            s.book_updates,
            s.updates_per_second,
            s.opportunities_detected,
            s.opportunities_per_hour,
            s.total_intents,
            s.would_full_fill,
            percent(s.would_full_fill, s.total_intents),
            s.would_partial_fill,
            percent(s.would_partial_fill, s.total_intents),
            s.would_not_fill,
            percent(s.would_not_fill, s.total_intents),
            s.fill_rate * 100.0,
            s.arb_attempts,
            s.arb_both_legs_fill,
            percent(s.arb_both_legs_fill, s.arb_attempts),
            s.arb_one_leg_only,
            percent(s.arb_one_leg_only, s.arb_attempts),
            s.arb_neither_leg,
            percent(s.arb_neither_leg, s.arb_attempts),
            s.arb_success_rate * 100.0,
            format_decimal(s.gross_edge_captured),
            format_decimal(s.taker_fees),
            format_decimal_signed(s.net_pnl_taker),
            verdict_taker,
            format_decimal_signed(s.projected_daily_pnl_taker),
            format_decimal(s.maker_fees),
            format_decimal_signed(s.net_pnl_maker),
            verdict_maker,
            format_decimal_signed(s.projected_daily_pnl_maker),
            s.avg_edge_percent,
            s.min_edge_percent,
            s.max_edge_percent,
            s.avg_slippage_cents,
            s.max_slippage_cents,
            verdict_taker,
            verdict_maker,
            recommendation,
        )
    }

    /// Generate short summary for heartbeat logging
    pub fn to_heartbeat(&self) -> String {
        let s = &self.summary;
        format!(
            "Paper: {} opps | {} arbs ({} ok, {} partial) | Taker: ${} | Maker: ${}",
            s.opportunities_detected,
            s.arb_attempts,
            s.arb_both_legs_fill,
            s.arb_one_leg_only,
            format_decimal_signed(s.net_pnl_taker),
            format_decimal_signed(s.net_pnl_maker),
        )
    }

    /// Generate JSON for programmatic analysis
    pub fn to_json(&self) -> serde_json::Value {
        let s = &self.summary;

        serde_json::json!({
            "session": {
                "duration_secs": s.session_duration.as_secs(),
                "duration_human": format_duration(s.session_duration),
                "book_updates": s.book_updates,
                "updates_per_second": s.updates_per_second
            },
            "opportunities": {
                "detected": s.opportunities_detected,
                "per_hour": s.opportunities_per_hour
            },
            "fills": {
                "total_intents": s.total_intents,
                "would_full_fill": s.would_full_fill,
                "would_partial_fill": s.would_partial_fill,
                "would_not_fill": s.would_not_fill,
                "fill_rate": s.fill_rate
            },
            "arbs": {
                "attempts": s.arb_attempts,
                "both_legs_fill": s.arb_both_legs_fill,
                "one_leg_only": s.arb_one_leg_only,
                "neither_leg": s.arb_neither_leg,
                "success_rate": s.arb_success_rate,
                "partial_arb_rate": s.partial_arb_rate
            },
            "pnl": {
                "gross_edge_captured": s.gross_edge_captured.to_string(),
                "taker": {
                    "fees": s.taker_fees.to_string(),
                    "net_pnl": s.net_pnl_taker.to_string(),
                    "projected_daily": s.projected_daily_pnl_taker.to_string(),
                    "profitable": s.taker_profitable
                },
                "maker": {
                    "fees": s.maker_fees.to_string(),
                    "net_pnl": s.net_pnl_maker.to_string(),
                    "projected_daily": s.projected_daily_pnl_maker.to_string(),
                    "profitable": s.maker_profitable
                }
            },
            "edge_analysis": {
                "avg_percent": s.avg_edge_percent,
                "min_percent": s.min_edge_percent,
                "max_percent": s.max_edge_percent
            },
            "slippage": {
                "avg_cents": s.avg_slippage_cents,
                "max_cents": s.max_slippage_cents
            },
            "verdict": {
                "taker_profitable": s.taker_profitable,
                "maker_profitable": s.maker_profitable,
                "recommendation": if s.maker_profitable && !s.taker_profitable {
                    "maker_only"
                } else if s.taker_profitable && s.maker_profitable {
                    "either_prefer_maker"
                } else if s.taker_profitable {
                    "taker_only"
                } else {
                    "neither_profitable"
                }
            }
        })
    }

    /// Save report to files (both console and JSON)
    pub fn save(&self, dir: &Path, prefix: &str) -> std::io::Result<()> {
        // Create directory if it doesn't exist
        fs::create_dir_all(dir)?;

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");

        // Save console report
        let console_path = dir.join(format!("{}_{}.txt", prefix, timestamp));
        let mut console_file = fs::File::create(&console_path)?;
        console_file.write_all(self.to_console().as_bytes())?;

        // Save JSON report
        let json_path = dir.join(format!("{}_{}.json", prefix, timestamp));
        let mut json_file = fs::File::create(&json_path)?;
        let json = serde_json::to_string_pretty(&self.to_json())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        json_file.write_all(json.as_bytes())?;

        Ok(())
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, mins, secs)
    } else if mins > 0 {
        format!("{}m {}s", mins, secs)
    } else {
        format!("{}s", secs)
    }
}

fn format_decimal(d: Decimal) -> String {
    format!("{:.2}", d)
}

fn format_decimal_signed(d: Decimal) -> String {
    if d >= Decimal::ZERO {
        format!("+{:.2}", d)
    } else {
        format!("{:.2}", d)
    }
}

fn percent(num: u64, denom: u64) -> f64 {
    if denom == 0 {
        0.0
    } else {
        (num as f64 / denom as f64) * 100.0
    }
}

// ============================================================================
// CSV EXPORT (for deeper analysis)
// ============================================================================

/// Export arb data to CSV for external analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct ArbRecord {
    pub timestamp: String,
    pub group_id: String,
    pub leg1_token: String,
    pub leg1_price: String,
    pub leg1_size: String,
    pub leg1_filled: bool,
    pub leg2_token: String,
    pub leg2_price: String,
    pub leg2_size: String,
    pub leg2_filled: bool,
    pub both_filled: bool,
    pub combined_cost: String,
    pub gross_edge: String,
    pub gross_edge_percent: String,
    pub net_pnl_taker: String,
    pub net_pnl_maker: String,
}

impl PaperReport {
    /// Export arb history to CSV (for detailed analysis)
    /// TODO: Add csv crate to dependencies to enable this
    pub fn export_arbs_csv(
        _arbs: &[crate::paper::fill_simulator::ArbSimulation],
        _path: &Path,
    ) -> std::io::Result<()> {
        // Temporarily disabled - need to add csv crate
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "CSV export not yet implemented - add csv crate to Cargo.toml"
        ))
        
        /* TODO: Enable when csv crate is added
        let mut wtr = csv::Writer::from_path(path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        for arb in arbs {
            let record = ArbRecord {
                timestamp: format!("{:?}", arb.timestamp),
                group_id: arb.group_id.clone(),
                leg1_token: arb.leg1.intent.token_id.clone(),
                leg1_price: arb.leg1.fill_price.map(|p| p.to_string()).unwrap_or_default(),
                leg1_size: arb.leg1.fill_size.map(|s| s.to_string()).unwrap_or_default(),
                leg1_filled: arb.leg1.would_fill(),
                leg2_token: arb.leg2.intent.token_id.clone(),
                leg2_price: arb.leg2.fill_price.map(|p| p.to_string()).unwrap_or_default(),
                leg2_size: arb.leg2.fill_size.map(|s| s.to_string()).unwrap_or_default(),
                leg2_filled: arb.leg2.would_fill(),
                both_filled: arb.both_would_fill,
                combined_cost: arb.combined_cost.to_string(),
                gross_edge: arb.gross_edge.to_string(),
                gross_edge_percent: arb.gross_edge_percent.to_string(),
                net_pnl_taker: arb.net_pnl_taker.to_string(),
                net_pnl_maker: arb.net_pnl_maker.to_string(),
            };

            wtr.serialize(record)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        }

        wtr.flush()?;
        Ok(())
        */
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paper::analytics::AnalyticsSummary;
    use rust_decimal_macros::dec;
    use std::time::Duration;

    fn make_summary() -> AnalyticsSummary {
        AnalyticsSummary {
            session_duration: Duration::from_secs(3600),
            book_updates: 36000,
            updates_per_second: 10.0,
            opportunities_detected: 50,
            opportunities_per_hour: 50.0,
            total_intents: 100,
            would_full_fill: 80,
            would_partial_fill: 10,
            would_not_fill: 10,
            fill_rate: 0.9,
            arb_attempts: 50,
            arb_both_legs_fill: 40,
            arb_one_leg_only: 5,
            arb_neither_leg: 5,
            arb_success_rate: 0.8,
            partial_arb_rate: 0.1,
            gross_edge_captured: dec!(20),
            taker_fees: dec!(60),
            maker_fees: Decimal::ZERO,
            net_pnl_taker: dec!(-40),
            net_pnl_maker: dec!(20),
            projected_daily_pnl_taker: dec!(-960),
            projected_daily_pnl_maker: dec!(480),
            avg_edge_percent: 1.2,
            min_edge_percent: 0.8,
            max_edge_percent: 2.1,
            avg_slippage_cents: 0.3,
            max_slippage_cents: 1.5,
            position_summary: None,
            taker_profitable: false,
            maker_profitable: true,
        }
    }

    #[test]
    fn test_console_report() {
        let summary = make_summary();
        let report = PaperReport::new(summary);
        let console = report.to_console();

        assert!(console.contains("PAPER TRADING REPORT"));
        assert!(console.contains("MAKER MODE"));
        assert!(console.contains("TAKER MODE"));
        assert!(console.contains("PROFITABLE"));
    }

    #[test]
    fn test_json_report() {
        let summary = make_summary();
        let report = PaperReport::new(summary);
        let json = report.to_json();

        assert!(json.get("session").is_some());
        assert!(json.get("pnl").is_some());
        assert!(json.get("verdict").is_some());

        let verdict = json.get("verdict").unwrap();
        assert_eq!(verdict.get("maker_profitable").unwrap(), true);
        assert_eq!(verdict.get("taker_profitable").unwrap(), false);
    }

    #[test]
    fn test_heartbeat() {
        let summary = make_summary();
        let report = PaperReport::new(summary);
        let heartbeat = report.to_heartbeat();

        assert!(heartbeat.contains("50 opps"));
        assert!(heartbeat.contains("50 arbs"));
    }
}
