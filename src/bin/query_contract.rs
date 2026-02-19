//! Standalone binary to query the Lightning Liquidity Manager contract state.
//!
//! Usage:
//!   cargo run --bin query-contract [script_address]
//!
//! Environment variables:
//!   BLOCKFROST_BASE_URL  - API base URL (default: http://localhost:8080/api/v1/)
//!   BLOCKFROST_PROJECT_ID - API key (default: "local" for yaci-devkit)
//!   SCRIPT_ADDRESS       - Contract script address (overridden by CLI arg)

// Allow this binary to use the cardano module from the main crate
use cardano_lightning_relay::cardano;

#[tokio::main]
async fn main() {
    let base_url = std::env::var("BLOCKFROST_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8080/api/v1/".to_string());

    let api_key =
        std::env::var("BLOCKFROST_PROJECT_ID").unwrap_or_else(|_| "local".to_string());

    let script_address = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::var("SCRIPT_ADDRESS").unwrap_or_else(|_| {
            eprintln!(
                "Usage: query-contract <script_address>\n\
                 Or set SCRIPT_ADDRESS env var."
            );
            std::process::exit(1);
        })
    });

    println!("Querying contract state...");
    println!("  API:     {}", base_url);
    println!("  Address: {}", script_address);
    println!();

    match cardano::query::query_state(&base_url, &api_key, &script_address).await {
        Ok(state) => {
            println!("{}", state);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
