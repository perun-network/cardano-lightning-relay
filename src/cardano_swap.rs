//! Cardano swap logic — connects Lightning payments to LM contract operations.
//!
//! `request_swap()` creates an LM invoice + BOLT11 invoice and stores the mapping.
//! `fulfill_swap()` is called when a Lightning payment is claimed, fulfilling the
//! LM invoice and sending cBTC to the user's Cardano address.

use crate::cardano_ops::CardanoOperator;
use crate::helpers::{current_timestamp_ms, query_state_with_retry, submit_contract_tx_with_retry};
use crate::mapping::{SwapDb, SwapMapping, SwapStatus};
use std::sync::Arc;
use std::time::Duration;

/// Swap description prefix used in BOLT11 invoices for swap detection.
const SWAP_PREFIX: &str = "cBTC_SWAP:";

/// Create a swap request: creates an LM invoice on Cardano and returns the
/// LM invoice ID (the caller is responsible for creating the BOLT11 invoice
/// and storing it in the swap DB).
///
/// Returns `(invoice_id, bolt11_description, create_tx_hash)` on success.
pub(crate) async fn request_swap(
	operator: &impl CardanoOperator,
	amount_cbtc: i64,
	cardano_address: &str,
	expiry_ms: i64,
) -> Result<(i64, String, String), String> {
	// Validate address and check network matches operator (testnet vs mainnet)
	let expected_prefix = if operator.operator_address().starts_with("addr_test") {
		Some("addr_test")
	} else {
		Some("addr")
	};
	let owner_pkh = validate_address(cardano_address, expected_prefix)?;

	let now_ms = current_timestamp_ms();
	let expires_at = now_ms + expiry_ms;

	// Create LM invoice on Cardano (retry on transient Blockfrost-lag errors)
	let (invoice_id, tx_hash) = submit_contract_tx_with_retry(
		"CreateInvoice",
		3,
		Duration::from_secs(15),
		|| async {
			let (id, tx) = operator.create_invoice(amount_cbtc, &owner_pkh, now_ms, expires_at).await?;
			let hash = operator.submit_tx(&tx).await?;
			Ok((id, hash))
		},
	)
	.await
	.map_err(|e| format!("failed to create invoice on Cardano: {}", e))?;

	println!("Created LM invoice #{} on Cardano (tx: {})", invoice_id, tx_hash);

	// The BOLT11 description encodes the Cardano address for swap detection
	let description = format!("{}{}", SWAP_PREFIX, cardano_address);

	Ok((invoice_id, description, tx_hash))
}

/// Store a swap mapping after the BOLT11 invoice has been created.
pub(crate) fn store_swap_mapping(
	swap_db: &Arc<SwapDb>,
	payment_hash: &str,
	invoice_id: i64,
	amount_cbtc: i64,
	cardano_address: &str,
	expires_at: i64,
	create_tx_hash: &str,
) {
	swap_db.insert(&SwapMapping {
		payment_hash: payment_hash.to_string(),
		invoice_id,
		amount_cbtc,
		cardano_address: cardano_address.to_string(),
		status: SwapStatus::Pending,
		created_at: current_timestamp_ms(),
		expires_at,
		cardano_tx_hash: None,
		create_tx_hash: Some(create_tx_hash.to_string()),
	});
}

/// Fulfill a swap after Lightning payment is claimed.
///
/// Looks up the mapping, builds + submits a FulfillInvoice tx on Cardano,
/// and updates the mapping status.
pub(crate) async fn fulfill_swap(
	operator: Arc<impl CardanoOperator>,
	swap_db: Arc<SwapDb>,
	payment_hash: String,
) {
	let mapping = match swap_db.get_by_payment_hash(&payment_hash) {
		Some(m) => m,
		None => return, // Not a swap payment, ignore
	};

	// Atomic transition: only proceed if still Pending (prevents double-fulfillment)
	if !swap_db.transition_status(&payment_hash, SwapStatus::Pending, SwapStatus::Fulfilling, None) {
		println!("Swap {} not in Pending status, skipping (concurrent or already processed)", payment_hash);
		return;
	}

	// Check if the on-chain invoice has expired
	if current_timestamp_ms() > mapping.expires_at {
		println!("Swap {} expired (expires_at: {}), marking failed", payment_hash, mapping.expires_at);
		swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
		return;
	}

	// Query the current state to find the invoice (retry while TX confirms).
	let invoice_id = mapping.invoice_id;
	let invoice = match query_state_with_retry(
		&*operator,
		24,
		std::time::Duration::from_secs(10),
		&format!("LM invoice #{}", invoice_id),
		|state| state.invoices.iter().find(|i| i.invoice_id == invoice_id).cloned(),
	)
	.await
	{
		Ok(i) => i,
		Err(e) => {
			println!("ERROR: {} for swap {}", e, payment_hash);
			swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
			return;
		},
	};

	// Build and submit the FulfillInvoice tx (retry on transient Blockfrost-lag errors)
	let result = submit_contract_tx_with_retry(
		&format!("FulfillInvoice({})", &payment_hash[..16]),
		3,
		Duration::from_secs(15),
		|| async {
			let signed_tx = operator.fulfill_invoice(&invoice, &mapping.cardano_address).await?;
			operator.submit_tx(&signed_tx).await
		},
	)
	.await;

	match result {
		Ok(tx_hash) => {
			println!("SUCCESS: swap {} fulfilled, cBTC sent to {}, tx: {}", payment_hash, mapping.cardano_address, tx_hash);
			swap_db.update_status(&payment_hash, SwapStatus::Completed, Some(&tx_hash));
		},
		Err(e) => {
			println!("ERROR: swap {} failed: {}", payment_hash, e);
			swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
		},
	}
}

/// Extract Cardano address from a BOLT11 invoice description.
/// Returns Some(address) if it matches the swap prefix.
pub(crate) fn extract_swap_address(description: &str) -> Option<String> {
	description
		.strip_prefix(SWAP_PREFIX)
		.map(|addr| addr.to_string())
}

/// Derive payment key hash from a bech32 Cardano address.
/// Validate a Cardano address and extract the payment key hash.
///
/// Accepts `addr_test1...` (testnet/preprod) and `addr1...` (mainnet).
/// Optionally validates the network prefix matches the expected network.
pub(crate) fn address_to_pkh(address: &str) -> Result<String, String> {
	validate_address(address, None)
}

/// Validate a Cardano address, check network prefix, and extract PKH.
pub(crate) fn validate_address(address: &str, expected_prefix: Option<&str>) -> Result<String, String> {
	use bech32::FromBase32;
	// Shelley addresses: 1-byte header + 28-byte PKH + 28-byte stake part
	let (hrp, data5, _variant) = bech32::decode(address)
		.map_err(|e| format!("invalid bech32 address: {}", e))?;

	if !hrp.starts_with("addr") {
		return Err(format!("not a Cardano address (hrp: {})", hrp));
	}

	// Network validation: addr_test for testnet/preprod, addr for mainnet
	if let Some(prefix) = expected_prefix {
		if !hrp.starts_with(prefix) {
			return Err(format!(
				"address network mismatch: expected {} prefix, got {}",
				prefix, hrp,
			));
		}
	}

	let data = Vec::<u8>::from_base32(&data5)
		.map_err(|e| format!("bech32 base32 decode failed: {:?}", e))?;

	if data.len() < 29 {
		return Err(format!("address too short: {} bytes", data.len()));
	}

	// Full Shelley address should be 57 bytes (1 header + 28 PKH + 28 stake)
	if data.len() != 57 && data.len() != 29 {
		return Err(format!(
			"unexpected address length: {} bytes (expected 29 for enterprise or 57 for base address)",
			data.len(),
		));
	}

	// Bytes 1..29 are the payment key hash
	let pkh = hex::encode(&data[1..29]);
	Ok(pkh)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Build a bech32 Cardano address from raw bytes for testing.
	fn encode_addr(hrp: &str, header: u8, pkh: &[u8; 28], stake: &[u8; 28]) -> String {
		use bech32::{ToBase32, Variant};
		let mut data = vec![header];
		data.extend_from_slice(pkh);
		data.extend_from_slice(stake);
		bech32::encode(hrp, data.to_base32(), Variant::Bech32).unwrap()
	}

	#[test]
	fn address_to_pkh_valid_testnet() {
		let pkh = [0xab; 28];
		let stake = [0xcd; 28];
		// header 0x00 = type-0 base address, testnet
		let addr = encode_addr("addr_test", 0x00, &pkh, &stake);

		let result = address_to_pkh(&addr).unwrap();
		assert_eq!(result, "ab".repeat(28));
	}

	#[test]
	fn address_to_pkh_valid_mainnet() {
		let pkh = [0x01; 28];
		let stake = [0x02; 28];
		// header 0x01 = type-0 base address, mainnet
		let addr = encode_addr("addr", 0x01, &pkh, &stake);

		let result = address_to_pkh(&addr).unwrap();
		assert_eq!(result, "01".repeat(28));
	}

	#[test]
	fn address_to_pkh_invalid_bech32() {
		let result = address_to_pkh("not-a-valid-address!!!");
		assert!(result.is_err());
		assert!(result.unwrap_err().contains("invalid bech32"));
	}

	#[test]
	fn address_to_pkh_wrong_prefix() {
		// Valid bech32 but with Bitcoin HRP, not Cardano
		use bech32::{ToBase32, Variant};
		let data = vec![0u8; 57];
		let addr = bech32::encode("bc", data.to_base32(), Variant::Bech32).unwrap();

		let result = address_to_pkh(&addr);
		assert!(result.is_err());
		assert!(result.unwrap_err().contains("not a Cardano address"));
	}

	#[test]
	fn extract_swap_address_with_prefix() {
		let desc = "cBTC_SWAP:addr_test1qz_something";
		assert_eq!(extract_swap_address(desc), Some("addr_test1qz_something".to_string()));
	}

	#[test]
	fn extract_swap_address_no_prefix() {
		assert_eq!(extract_swap_address("random description"), None);
	}
}
