//! Cardano offramp logic — cBTC → Lightning BTC (contract-integrated).
//!
//! Flow:
//!   1. `request_offramp()` — submit CreateOfframp TX, return offramp_id + operator_address
//!   2. User sends cBTC to operator address (off-contract Cardano TX)
//!   3. `process_offramp_deposit()` — verify cBTC receipt, pay user's BOLT11 invoice
//!   4. `complete_offramp()` — submit FulfillOfframp TX (deposit cBTC to pool)
//!   5. `handle_offramp_payment_failed()` — submit CancelOfframp TX on failure

use crate::cardano_ops::CardanoOperator;
use crate::cardano_swap::address_to_pkh;
use crate::cli::payment_cmds;
use crate::helpers::{current_timestamp_ms, query_state_with_retry};
use crate::mapping::{OfframpMapping, OfframpStatus, SwapDb};
use crate::types::{ChannelManager, OutboundPaymentInfoStorage};
use cardano_lightning_client::OperatorAgent;
use lightning_invoice::Bolt11Invoice;
use lightning_persister::fs_store::FilesystemStore;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// Process an offramp request: submit CreateOfframp TX on-chain, return offramp details.
///
/// Returns `(offramp_id, operator_address, payment_hash)` on success.
pub(crate) async fn request_offramp(
	operator: &impl CardanoOperator,
	swap_db: &Arc<SwapDb>,
	bolt11_str: &str,
	amount_cbtc: i64,
	cardano_address: &str,
) -> Result<(i64, String, String), String> {
	// 1. Parse BOLT11 invoice
	let invoice = Bolt11Invoice::from_str(bolt11_str)
		.map_err(|e| format!("invalid BOLT11 invoice: {:?}", e))?;

	let payment_hash = format!("{}", invoice.payment_hash());

	// Reject reuse of the same BOLT11 invoice (same payment_hash)
	if let Some(existing) = swap_db.get_offramp_by_payment_hash(&payment_hash) {
		return Err(format!(
			"BOLT11 invoice already used in offramp #{} (status: {:?})",
			existing.offramp_id, existing.status,
		));
	}

	// Validate amount matches (1:1 cBTC = msat for now)
	if let Some(inv_amt) = invoice.amount_milli_satoshis() {
		if inv_amt != amount_cbtc as u64 {
			return Err(format!(
				"invoice amount {} msat does not match requested {} cBTC",
				inv_amt, amount_cbtc,
			));
		}
	}

	// 2. Derive refund_address PKH from cardano_address
	let refund_pkh = address_to_pkh(cardano_address)?;

	// 3. Set expiry (1 hour from now)
	let now_ms = current_timestamp_ms();
	let expires_at = now_ms + 3_600_000;

	// 4. Submit CreateOfframp TX on-chain
	let (offramp_id, signed_tx) = operator
		.create_offramp(amount_cbtc, &payment_hash, &refund_pkh, expires_at)
		.await
		.map_err(|e| format!("failed to build CreateOfframp tx: {}", e))?;

	let create_tx_hash = operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| format!("failed to submit CreateOfframp tx: {}", e))?;

	println!(
		"Offramp #{}: CreateOfframp TX submitted (hash: {})",
		offramp_id, create_tx_hash,
	);

	// 5. Store offramp mapping with AwaitingDeposit status
	let operator_address = operator.operator_address().to_string();

	swap_db.insert_offramp(&OfframpMapping {
		offramp_id,
		bolt11: bolt11_str.to_string(),
		payment_hash: payment_hash.clone(),
		amount_cbtc,
		cbtc_tx_hash: String::new(), // not yet known
		status: OfframpStatus::AwaitingDeposit,
		created_at: now_ms,
		lightning_preimage: None,
		deposit_tx_hash: None,
		error_message: None,
		cardano_offramp_tx_hash: Some(create_tx_hash),
		refund_address: refund_pkh,
		expires_at,
	});

	Ok((offramp_id, operator_address, payment_hash))
}

/// Process a user's cBTC deposit notification: verify cBTC, then pay Lightning invoice.
pub(crate) async fn process_offramp_deposit(
	operator: &Arc<OperatorAgent>,
	swap_db: &Arc<SwapDb>,
	channel_manager: &Arc<ChannelManager>,
	outbound_payments: &Arc<Mutex<OutboundPaymentInfoStorage>>,
	fs_store: &Arc<FilesystemStore>,
	offramp_id: i64,
	cbtc_tx_hash: &str,
) -> Result<(), String> {
	let mapping = swap_db.get_offramp_by_id(offramp_id)
		.ok_or_else(|| format!("offramp {} not found", offramp_id))?;

	// Reject reuse of a cBTC TX hash already claimed by another offramp
	if let Some(existing) = swap_db.get_offramp_by_cbtc_tx(cbtc_tx_hash) {
		if existing.offramp_id != offramp_id {
			return Err(format!(
				"cBTC TX {} already used by offramp #{}",
				cbtc_tx_hash, existing.offramp_id,
			));
		}
	}

	// Atomic status transition: only proceed if still AwaitingDeposit (prevents double-fulfillment)
	if !swap_db.transition_offramp_status(
		offramp_id, OfframpStatus::AwaitingDeposit, OfframpStatus::PendingVerification,
	) {
		return Err(format!(
			"offramp {} is not in AwaitingDeposit status (concurrent request or already processed)",
			offramp_id,
		));
	}

	// 1. Verify cBTC arrived at operator address BEFORE storing the TX hash
	let received = operator
		.verify_cbtc_received(cbtc_tx_hash, mapping.amount_cbtc)
		.await
		.map_err(|e| {
			// Revert status on verification error
			swap_db.update_offramp_status(
				offramp_id, OfframpStatus::AwaitingDeposit, None, None, None,
			);
			format!("failed to verify cBTC receipt: {}", e)
		})?;

	if !received {
		swap_db.update_offramp_status(
			offramp_id, OfframpStatus::AwaitingDeposit, None, None,
			Some("cBTC not received at operator address"),
		);
		return Err(format!(
			"cBTC not found at operator address in tx {}",
			cbtc_tx_hash,
		));
	}

	// Store verified TX hash and transition to PayingLightning
	swap_db.update_offramp_cbtc_tx(offramp_id, cbtc_tx_hash);
	swap_db.update_offramp_status(
		offramp_id, OfframpStatus::PayingLightning, None, None, None,
	);

	println!("Offramp #{}: paying Lightning invoice (hash: {})", offramp_id, mapping.payment_hash);

	let invoice = Bolt11Invoice::from_str(&mapping.bolt11)
		.map_err(|e| format!("failed to re-parse bolt11: {:?}", e))?;

	payment_cmds::send_payment(
		channel_manager,
		&invoice,
		None,
		outbound_payments,
		fs_store,
	)
	.await;

	Ok(())
}

/// Called from PaymentSent event: submit FulfillOfframp TX to deposit cBTC to pool.
pub(crate) async fn complete_offramp(
	operator: Arc<impl CardanoOperator>,
	swap_db: Arc<SwapDb>,
	payment_hash: String,
	preimage: String,
) {
	let mapping = match swap_db.get_offramp_by_payment_hash(&payment_hash) {
		Some(m) => m,
		None => return,
	};

	if mapping.status != OfframpStatus::PayingLightning {
		println!(
			"Offramp #{} already in status {:?}, skipping",
			mapping.offramp_id, mapping.status,
		);
		return;
	}

	swap_db.update_offramp_status(
		mapping.offramp_id,
		OfframpStatus::DepositingToPool,
		Some(&preimage),
		None,
		None,
	);

	println!(
		"Offramp #{}: Lightning payment sent (preimage: {}), submitting FulfillOfframp TX...",
		mapping.offramp_id, preimage,
	);

	// Query on-chain state to find the offramp entry for FulfillOfframp
	let offramp_id = mapping.offramp_id;
	let offramp = match query_state_with_retry(
		&*operator,
		6,
		std::time::Duration::from_secs(5),
		&format!("Offramp #{}", offramp_id),
		|state| state.offramps.iter().find(|o| o.offramp_id == offramp_id).cloned(),
	)
	.await
	{
		Ok(o) => o,
		Err(e) => {
			let msg = format!("failed to find on-chain offramp: {}", e);
			println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
			swap_db.update_offramp_status(
				mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
			);
			return;
		},
	};

	// Build and submit FulfillOfframp TX
	let signed_tx = match operator.fulfill_offramp(&offramp).await {
		Ok(tx) => tx,
		Err(e) => {
			let msg = format!("failed to build FulfillOfframp tx: {}", e);
			println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
			swap_db.update_offramp_status(
				mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
			);
			return;
		},
	};

	match operator.submit_tx(&signed_tx).await {
		Ok(tx_hash) => {
			println!(
				"SUCCESS: Offramp #{} completed, FulfillOfframp TX: {}",
				mapping.offramp_id, tx_hash,
			);
			swap_db.update_offramp_status(
				mapping.offramp_id, OfframpStatus::Completed, None, Some(&tx_hash), None,
			);
		},
		Err(e) => {
			let msg = format!("failed to submit FulfillOfframp tx: {}", e);
			println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
			swap_db.update_offramp_status(
				mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
			);
		},
	}
}

/// Called from PaymentFailed event: mark as failed and schedule CancelOfframp.
pub(crate) async fn handle_offramp_payment_failed(
	operator: Arc<impl CardanoOperator>,
	swap_db: Arc<SwapDb>,
	payment_hash: String,
) {
	let mapping = match swap_db.get_offramp_by_payment_hash(&payment_hash) {
		Some(m) => m,
		None => return,
	};

	println!(
		"ERROR: Offramp #{}: Lightning payment failed, attempting cBTC refund to {}",
		mapping.offramp_id, mapping.refund_address,
	);

	// Try to refund cBTC to user's refund address (best-effort)
	if !mapping.refund_address.is_empty() {
		match operator.send_cbtc(&mapping.refund_address, mapping.amount_cbtc).await {
			Ok(signed_tx) => {
				match operator.submit_tx(&signed_tx).await {
					Ok(tx_hash) => {
						let msg = format!("Lightning payment failed, cBTC refunded: {}", tx_hash);
						println!("Offramp #{}: {}", mapping.offramp_id, msg);
						swap_db.update_offramp_status(
							mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
						);
					},
					Err(e) => {
						let msg = format!("Lightning payment failed, refund submit failed: {}", e);
						println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
						swap_db.update_offramp_status(
							mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
						);
					},
				}
			},
			Err(e) => {
				let msg = format!("Lightning payment failed, refund build failed: {}", e);
				println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
				swap_db.update_offramp_status(
					mapping.offramp_id, OfframpStatus::Failed, None, None, Some(&msg),
				);
			},
		}
	} else {
		let msg = "Lightning payment failed, no refund address available";
		println!("ERROR: Offramp #{}: {}", mapping.offramp_id, msg);
		swap_db.update_offramp_status(
			mapping.offramp_id, OfframpStatus::Failed, None, None, Some(msg),
		);
	}

	// Also try to cancel the on-chain offramp entry (best-effort)
	cancel_offramp_on_chain(&*operator, &swap_db, &mapping).await;
}

/// Submit CancelOfframp TX for a single offramp. Logs success/failure.
pub(crate) async fn cancel_offramp_on_chain(
	operator: &impl CardanoOperator,
	swap_db: &SwapDb,
	mapping: &OfframpMapping,
) {
	let signed_tx = match operator.cancel_offramp(mapping.offramp_id).await {
		Ok(tx) => tx,
		Err(e) => {
			println!(
				"Offramp #{}: CancelOfframp not yet possible (expires_at: {}): {}",
				mapping.offramp_id, mapping.expires_at, e,
			);
			return;
		},
	};
	match operator.submit_tx(&signed_tx).await {
		Ok(tx_hash) => {
			println!(
				"SUCCESS: Offramp #{} cancelled on-chain, tx: {}",
				mapping.offramp_id, tx_hash,
			);
			swap_db.update_offramp_status(
				mapping.offramp_id, OfframpStatus::Failed, None, None,
				Some(&format!("cancelled on-chain: {}", tx_hash)),
			);
		},
		Err(e) => {
			println!(
				"Offramp #{}: failed to submit CancelOfframp tx: {}",
				mapping.offramp_id, e,
			);
		},
	}
}
