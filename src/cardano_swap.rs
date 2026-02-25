//! Cardano swap logic — connects Lightning payments to LM contract operations.
//!
//! `request_swap()` creates an LM invoice + BOLT11 invoice and stores the mapping.
//! `fulfill_swap()` is called when a Lightning payment is claimed, fulfilling the
//! LM invoice and sending cBTC to the user's Cardano address.

use crate::mapping::{SwapDb, SwapMapping, SwapStatus};
use cardano_lightning_client::OperatorAgent;
use std::sync::Arc;

/// Swap description prefix used in BOLT11 invoices for swap detection.
const SWAP_PREFIX: &str = "cBTC_SWAP:";

/// Create a swap request: creates an LM invoice on Cardano and returns the
/// LM invoice ID (the caller is responsible for creating the BOLT11 invoice
/// and storing it in the swap DB).
///
/// Returns `(invoice_id, bolt11_description)` on success.
pub(crate) async fn request_swap(
	operator: &Arc<OperatorAgent>,
	_swap_db: &Arc<SwapDb>,
	amount_cbtc: i64,
	cardano_address: &str,
) -> Result<(i64, String), String> {
	let owner_pkh = address_to_pkh(cardano_address)?;

	let now_ms = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64;

	// 1 hour expiry
	let expires_at = now_ms + 3_600_000;

	// Create LM invoice on Cardano
	let (invoice_id, signed_tx) = operator
		.create_invoice(amount_cbtc, &owner_pkh, now_ms, expires_at)
		.await
		.map_err(|e| format!("failed to build create_invoice tx: {}", e))?;

	let tx_hash = operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| format!("failed to submit create_invoice tx: {}", e))?;

	println!("Created LM invoice #{} on Cardano (tx: {})", invoice_id, tx_hash);

	// The BOLT11 description encodes the Cardano address for swap detection
	let description = format!("{}{}", SWAP_PREFIX, cardano_address);

	// We don't store the mapping yet — the caller creates the BOLT11 invoice
	// and then calls store_swap_mapping with the payment_hash.

	Ok((invoice_id, description))
}

/// Store a swap mapping after the BOLT11 invoice has been created.
pub(crate) fn store_swap_mapping(
	swap_db: &Arc<SwapDb>,
	payment_hash: &str,
	invoice_id: i64,
	amount_cbtc: i64,
	cardano_address: &str,
	expires_at: i64,
) {
	let now_ms = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64;

	swap_db.insert(&SwapMapping {
		payment_hash: payment_hash.to_string(),
		invoice_id,
		amount_cbtc,
		cardano_address: cardano_address.to_string(),
		status: SwapStatus::Pending,
		created_at: now_ms,
		expires_at,
		cardano_tx_hash: None,
	});
}

/// Fulfill a swap after Lightning payment is claimed.
///
/// Looks up the mapping, builds + submits a FulfillInvoice tx on Cardano,
/// and updates the mapping status.
pub(crate) async fn fulfill_swap(
	operator: Arc<OperatorAgent>,
	swap_db: Arc<SwapDb>,
	payment_hash: String,
) {
	let mapping = match swap_db.get_by_payment_hash(&payment_hash) {
		Some(m) => m,
		None => return, // Not a swap payment, ignore
	};

	if mapping.status != SwapStatus::Pending {
		println!("Swap {} already in status {:?}, skipping", payment_hash, mapping.status);
		return;
	}

	swap_db.update_status(&payment_hash, SwapStatus::Fulfilling, None);

	// Query the current state to find the invoice.
	// The CreateInvoice TX may not be confirmed yet, so retry a few times.
	let mut invoice = None;
	for attempt in 0..12 {
		match operator.agent().query_state().await {
			Ok(state) => {
				if let Some(i) = state.invoices.iter().find(|i| i.invoice_id == mapping.invoice_id) {
					invoice = Some(i.clone());
					break;
				}
				if attempt < 11 {
					println!("Waiting for LM invoice #{} to confirm on-chain (attempt {}/12)...", mapping.invoice_id, attempt + 1);
					tokio::time::sleep(std::time::Duration::from_secs(5)).await;
				}
			},
			Err(e) => {
				if attempt == 11 {
					println!("ERROR: failed to query pool state for swap {}: {}", payment_hash, e);
					swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
					return;
				}
				tokio::time::sleep(std::time::Duration::from_secs(5)).await;
			},
		}
	}

	let invoice = match invoice {
		Some(i) => i,
		None => {
			println!("ERROR: LM invoice #{} not found for swap {} after retries", mapping.invoice_id, payment_hash);
			swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
			return;
		},
	};

	// Build and submit the FulfillInvoice tx
	let signed_tx = match operator.fulfill_invoice(&invoice, &mapping.cardano_address).await {
		Ok(tx) => tx,
		Err(e) => {
			println!("ERROR: failed to build fulfill_invoice tx for swap {}: {}", payment_hash, e);
			swap_db.update_status(&payment_hash, SwapStatus::Failed, None);
			return;
		},
	};

	match operator.submit_tx(&signed_tx).await {
		Ok(tx_hash) => {
			println!("SUCCESS: swap {} fulfilled, cBTC sent to {}, tx: {}", payment_hash, mapping.cardano_address, tx_hash);
			swap_db.update_status(&payment_hash, SwapStatus::Completed, Some(&tx_hash));
		},
		Err(e) => {
			println!("ERROR: failed to submit fulfill_invoice tx for swap {}: {}", payment_hash, e);
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
pub(crate) fn address_to_pkh(address: &str) -> Result<String, String> {
	use bech32::FromBase32;
	// Shelley addresses: 1-byte header + 28-byte PKH + 28-byte stake part
	let (hrp, data5, _variant) = bech32::decode(address)
		.map_err(|e| format!("invalid bech32 address: {}", e))?;

	if !hrp.starts_with("addr") {
		return Err(format!("not a Cardano address (hrp: {})", hrp));
	}

	let data = Vec::<u8>::from_base32(&data5)
		.map_err(|e| format!("bech32 base32 decode failed: {:?}", e))?;

	if data.len() < 29 {
		return Err(format!("address too short: {} bytes", data.len()));
	}

	// Bytes 1..29 are the payment key hash
	let pkh = hex::encode(&data[1..29]);
	Ok(pkh)
}
