//! Blockfrost API queries for the Lightning Liquidity Manager contract.
//!
//! Works against both real Blockfrost and yaci-devkit's compatible API.

use super::types::{State, cbor_hex_to_plutus_json};
use serde_json::Value;

/// Query the current contract state from a Blockfrost-compatible API.
///
/// Fetches UTxOs at the script address, finds the one with an inline datum,
/// and parses the Plutus JSON datum into a `State`.
pub async fn query_state(
    base_url: &str,
    api_key: &str,
    script_address: &str,
) -> Result<State, String> {
    let client = reqwest::Client::new();

    // Normalize base URL (ensure trailing slash)
    let base = base_url.trim_end_matches('/');

    let url = format!("{}/addresses/{}/utxos", base, script_address);

    let resp = client
        .get(&url)
        .header("project_id", api_key)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Blockfrost API error {}: {}", status, body));
    }

    let utxos: Vec<Value> = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse UTxO response: {}", e))?;

    if utxos.is_empty() {
        return Err("No UTxOs found at script address".to_string());
    }

    // Find the UTxO with an inline datum (the contract state UTxO)
    for utxo in &utxos {
        if let Some(inline_datum) = utxo.get("inline_datum") {
            if inline_datum.is_null() {
                continue;
            }
            // Real Blockfrost returns Plutus JSON object; yaci-devkit returns CBOR hex string
            if let Some(cbor_hex) = inline_datum.as_str() {
                let plutus_json = cbor_hex_to_plutus_json(cbor_hex)?;
                return State::try_from(&plutus_json);
            }
            return State::try_from(inline_datum);
        }
    }

    // If no inline_datum field, try data_hash approach (fetch datum separately)
    for utxo in &utxos {
        if let Some(data_hash) = utxo.get("data_hash").and_then(|d| d.as_str()) {
            let datum_url = format!("{}/scripts/datum/{}", base, data_hash);
            let datum_resp = client
                .get(&datum_url)
                .header("project_id", api_key)
                .send()
                .await
                .map_err(|e| format!("Datum fetch failed: {}", e))?;

            if datum_resp.status().is_success() {
                let datum_wrapper: Value = datum_resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse datum: {}", e))?;

                if let Some(json_value) = datum_wrapper.get("json_value") {
                    return State::try_from(json_value);
                }
            }
        }
    }

    Err("No UTxO with inline datum found at script address".to_string())
}
