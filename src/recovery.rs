//! Crash recovery — retries stuck swaps and offramps on startup.
//!
//! If the relay crashes between status transitions (e.g., after receiving a Lightning
//! payment but before submitting the Cardano TX), swaps can get stuck. This module
//! scans for stuck entries and retries them.

use crate::cardano_ops::CardanoOperator;
use crate::helpers::query_state_with_retry;
use crate::mapping::{OfframpStatus, SwapDb, SwapStatus};

/// Recover swaps stuck in Fulfilling status.
///
/// These received a Lightning payment but the Cardano FulfillInvoice TX was never
/// submitted (or never confirmed). We query on-chain state: if the invoice is already
/// fulfilled, just update the DB. Otherwise, retry the TX.
pub(crate) async fn recover_fulfilling_swaps(
	operator: &impl CardanoOperator, swap_db: &SwapDb,
) {
	let stuck = swap_db.get_by_status(SwapStatus::Fulfilling);
	if stuck.is_empty() {
		return;
	}
	println!("Recovery: found {} swap(s) stuck in Fulfilling", stuck.len());

	for mapping in &stuck {
		println!("Recovery: retrying swap {} (invoice #{})",
			mapping.payment_hash, mapping.invoice_id);

		// Check if invoice still exists on-chain (if not, it was already fulfilled)
		let invoice_id = mapping.invoice_id;
		let invoice = match query_state_with_retry(
			operator, 3, std::time::Duration::from_secs(5),
			&format!("recovery invoice #{}", invoice_id),
			|state| state.invoices.iter().find(|i| i.invoice_id == invoice_id).cloned(),
		).await {
			Ok(i) => i,
			Err(_) => {
				// Invoice not found on-chain — likely already fulfilled
				println!("Recovery: invoice #{} not found on-chain, marking completed",
					invoice_id);
				swap_db.update_status(
					&mapping.payment_hash, SwapStatus::Completed, None,
				);
				continue;
			},
		};

		// Invoice still on-chain — retry FulfillInvoice
		let signed_tx = match operator.fulfill_invoice(&invoice, &mapping.cardano_address).await {
			Ok(tx) => tx,
			Err(e) => {
				println!("Recovery: failed to build fulfill tx for swap {}: {}",
					mapping.payment_hash, e);
				continue;
			},
		};

		match operator.submit_tx(&signed_tx).await {
			Ok(tx_hash) => {
				println!("Recovery: swap {} fulfilled, tx: {}", mapping.payment_hash, tx_hash);
				swap_db.update_status(
					&mapping.payment_hash, SwapStatus::Completed, Some(&tx_hash),
				);
			},
			Err(e) => {
				println!("Recovery: failed to submit fulfill tx for swap {}: {}",
					mapping.payment_hash, e);
			},
		}
	}
}

/// Recover offramps stuck in PendingVerification, PayingLightning, or DepositingToPool.
///
/// PendingVerification: Relay crashed during cBTC verification. Reset to AwaitingDeposit
/// so the user can retry the deposit notification.
///
/// PayingLightning: Lightning payment may or may not have been sent. Mark as failed
/// so the background expiry monitor can cancel it on-chain.
///
/// DepositingToPool: Lightning payment was sent but FulfillOfframp TX was never submitted.
/// Query on-chain state and retry.
pub(crate) async fn recover_depositing_offramps(
	operator: &impl CardanoOperator, swap_db: &SwapDb,
) {
	// Recover PendingVerification — reset to AwaitingDeposit so user can retry
	let verifying = swap_db.get_offramps_by_status(OfframpStatus::PendingVerification);
	if !verifying.is_empty() {
		println!("Recovery: found {} offramp(s) stuck in PendingVerification", verifying.len());
		for mapping in &verifying {
			if swap_db.transition_offramp_status(
				mapping.offramp_id, OfframpStatus::PendingVerification, OfframpStatus::AwaitingDeposit,
			) {
				println!("Recovery: offramp #{} reset to AwaitingDeposit (was stuck in PendingVerification)",
					mapping.offramp_id);
			}
		}
	}

	// Recover PayingLightning — we can't retry the Lightning payment on startup
	// (no invoice state preserved), so mark as failed for expiry handling.
	let paying = swap_db.get_offramps_by_status(OfframpStatus::PayingLightning);
	if !paying.is_empty() {
		println!("Recovery: found {} offramp(s) stuck in PayingLightning", paying.len());
		for mapping in &paying {
			// Atomic transition: skip if an event handler already moved this offramp
			if swap_db.transition_offramp_status(
				mapping.offramp_id, OfframpStatus::PayingLightning, OfframpStatus::Failed,
			) {
				println!("Recovery: offramp #{} stuck in PayingLightning, marked failed for expiry recovery",
					mapping.offramp_id);
				swap_db.update_offramp_status(
					mapping.offramp_id, OfframpStatus::Failed, None, None,
					Some("relay crashed during Lightning payment, awaiting expiry for on-chain cancel"),
				);
			} else {
				println!("Recovery: offramp #{} already moved from PayingLightning, skipping",
					mapping.offramp_id);
			}
		}
	}

	// Recover DepositingToPool
	let stuck = swap_db.get_offramps_by_status(OfframpStatus::DepositingToPool);
	if stuck.is_empty() {
		return;
	}
	println!("Recovery: found {} offramp(s) stuck in DepositingToPool", stuck.len());

	for mapping in &stuck {
		println!("Recovery: retrying offramp #{} (payment_hash: {})",
			mapping.offramp_id, mapping.payment_hash);

		let offramp_id = mapping.offramp_id;
		// Find the offramp on-chain
		let offramp = match query_state_with_retry(
			operator, 3, std::time::Duration::from_secs(5),
			&format!("recovery offramp #{}", offramp_id),
			|state| state.offramps.iter().find(|o| o.offramp_id == offramp_id).cloned(),
		).await {
			Ok(o) => o,
			Err(_) => {
				// Offramp not on-chain — already fulfilled
				println!("Recovery: offramp #{} not found on-chain, marking completed",
					offramp_id);
				swap_db.update_offramp_status(
					offramp_id, OfframpStatus::Completed, None, None, None,
				);
				continue;
			},
		};

		// Retry FulfillOfframp
		let signed_tx = match operator.fulfill_offramp(&offramp).await {
			Ok(tx) => tx,
			Err(e) => {
				println!("Recovery: failed to build FulfillOfframp tx for #{}: {}",
					offramp_id, e);
				continue;
			},
		};

		match operator.submit_tx(&signed_tx).await {
			Ok(tx_hash) => {
				println!("Recovery: offramp #{} fulfilled, tx: {}", offramp_id, tx_hash);
				swap_db.update_offramp_status(
					offramp_id, OfframpStatus::Completed, None, None, None,
				);
			},
			Err(e) => {
				println!("Recovery: failed to submit FulfillOfframp tx for #{}: {}",
					offramp_id, e);
			},
		}
	}
}
