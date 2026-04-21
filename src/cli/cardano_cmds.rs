//! CLI commands for Cardano LM contract operations.
//!
//! All TX-submitting commands use `submit_contract_tx_with_retry` to handle the
//! Blockfrost-lag pattern (BadInputs / already-included / ConwayMempoolFailure)
//! that occurs when the relay builds a TX referencing a UTxO that was just
//! consumed by another TX whose new state hasn't been indexed yet.
//!
//! Sequential cancel-loops also wait between successful cancels so each
//! subsequent TX builds on fresh state.

use crate::helpers::{current_timestamp_ms, submit_contract_tx_with_retry};
use cardano_lightning_client::OperatorAgent;
use std::sync::Arc;
use std::time::Duration;

/// Default settings for retrying contract TX submissions.
const RETRY_ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(15);

/// Wait between successful sequential cancels — gives Blockfrost time to index
/// the new script UTxO before we build the next cancel TX.
const SEQ_CANCEL_DELAY: Duration = Duration::from_secs(30);

/// Upper wall-clock budget for a single cancel-expired invocation. Prevents
/// the loop from running indefinitely when there are many stale entries and
/// every cancel is slow. Callers can re-invoke to process more.
const CANCEL_BUDGET: Duration = Duration::from_secs(180);

/// Show pool state from the LM contract.
pub(crate) async fn pool_info(operator: &Arc<OperatorAgent>) {
	match operator.agent().query_state().await {
		Ok(state) => println!("{}", state),
		Err(e) => println!("ERROR: failed to query pool state: {}", e),
	}
}

/// Deposit cBTC into the pool.
pub(crate) async fn cardano_deposit(operator: &Arc<OperatorAgent>, amount: i64) {
	println!("Building deposit tx for {} cBTC units...", amount);
	let result = submit_contract_tx_with_retry(
		&format!("Deposit({})", amount),
		RETRY_ATTEMPTS,
		RETRY_DELAY,
		|| async {
			let signed_tx = operator.deposit(amount).await?;
			operator.submit_tx(&signed_tx).await
		},
	)
	.await;
	match result {
		Ok(tx_hash) => println!("SUCCESS: deposit submitted, tx_hash: {}", tx_hash),
		Err(e) => println!("ERROR: {}", e),
	}
}

/// Withdraw cBTC from the pool.
pub(crate) async fn cardano_withdraw(operator: &Arc<OperatorAgent>, amount: i64) {
	println!("Building withdraw tx for {} cBTC units...", amount);
	let result = submit_contract_tx_with_retry(
		&format!("Withdraw({})", amount),
		RETRY_ATTEMPTS,
		RETRY_DELAY,
		|| async {
			let signed_tx = operator.withdraw(amount).await?;
			operator.submit_tx(&signed_tx).await
		},
	)
	.await;
	match result {
		Ok(tx_hash) => println!("SUCCESS: withdraw submitted, tx_hash: {}", tx_hash),
		Err(e) => println!("ERROR: {}", e),
	}
}

/// Cancel expired offramps on-chain.
///
/// Each cancel consumes the script UTxO and produces a new one. We wait between
/// successive cancels to let Blockfrost index the new state — otherwise the next
/// build would use stale UTxO and fail with BadInputs.
pub(crate) async fn cancel_expired_offramps(operator: &Arc<OperatorAgent>) {
	let state = match operator.agent().query_state().await {
		Ok(s) => s,
		Err(e) => {
			println!("ERROR: failed to query pool state: {}", e);
			return;
		},
	};

	let now_ms = current_timestamp_ms();
	let expired: Vec<_> = state.offramps.iter().filter(|o| o.expires_at < now_ms).collect();

	if expired.is_empty() {
		println!("No expired offramps found.");
		return;
	}

	println!("Found {} expired offramp(s), cancelling sequentially...", expired.len());
	let start = std::time::Instant::now();
	let mut cancelled = 0;
	let mut last_ok = false;
	for offramp in &expired {
		if start.elapsed() > CANCEL_BUDGET {
			println!(
				"Budget of {}s reached ({}/{} processed) — re-invoke to continue.",
				CANCEL_BUDGET.as_secs(), cancelled, expired.len(),
			);
			break;
		}
		// Only wait after a SUCCESSFUL cancel (script UTxO changed). A failed
		// cancel didn't touch state, so no need to wait.
		if last_ok {
			println!("  Waiting {}s for previous cancel to be indexed...", SEQ_CANCEL_DELAY.as_secs());
			tokio::time::sleep(SEQ_CANCEL_DELAY).await;
		}
		println!("  Cancelling offramp #{} (expired at {})...", offramp.offramp_id, offramp.expires_at);
		let result = submit_contract_tx_with_retry(
			&format!("CancelOfframp(#{})", offramp.offramp_id),
			RETRY_ATTEMPTS,
			RETRY_DELAY,
			|| async {
				let signed_tx = operator.cancel_offramp(offramp.offramp_id).await?;
				operator.submit_tx(&signed_tx).await
			},
		)
		.await;
		match result {
			Ok(tx_hash) => {
				println!("  SUCCESS: offramp #{} cancelled, tx_hash: {}", offramp.offramp_id, tx_hash);
				cancelled += 1;
				last_ok = true;
			},
			Err(e) => {
				println!("  ERROR: {}", e);
				last_ok = false;
			},
		}
	}
	println!("Cancelled {}/{} expired offramps.", cancelled, expired.len());
}

/// Cancel expired invoices in the pool.
///
/// Same sequential pattern as `cancel_expired_offramps` — waits between successful
/// cancels so each TX builds on fresh script UTxO state.
pub(crate) async fn cancel_expired(operator: &Arc<OperatorAgent>) {
	let state = match operator.agent().query_state().await {
		Ok(s) => s,
		Err(e) => {
			println!("ERROR: failed to query pool state: {}", e);
			return;
		},
	};

	let now_ms = current_timestamp_ms();
	let expired: Vec<_> = state.invoices.iter().filter(|i| i.expires_at < now_ms).collect();

	if expired.is_empty() {
		println!("No expired invoices found.");
		return;
	}

	println!("Found {} expired invoice(s), cancelling sequentially...", expired.len());
	let start = std::time::Instant::now();
	let mut cancelled = 0;
	let mut last_ok = false;
	for inv in &expired {
		if start.elapsed() > CANCEL_BUDGET {
			println!(
				"Budget of {}s reached ({}/{} processed) — re-invoke to continue.",
				CANCEL_BUDGET.as_secs(), cancelled, expired.len(),
			);
			break;
		}
		if last_ok {
			println!("  Waiting {}s for previous cancel to be indexed...", SEQ_CANCEL_DELAY.as_secs());
			tokio::time::sleep(SEQ_CANCEL_DELAY).await;
		}
		println!("  Cancelling invoice #{} (expired at {})...", inv.invoice_id, inv.expires_at);
		let result = submit_contract_tx_with_retry(
			&format!("CancelInvoice(#{})", inv.invoice_id),
			RETRY_ATTEMPTS,
			RETRY_DELAY,
			|| async {
				let signed_tx = operator.cancel_invoice(inv.invoice_id).await?;
				operator.submit_tx(&signed_tx).await
			},
		)
		.await;
		match result {
			Ok(tx_hash) => {
				println!("  SUCCESS: invoice #{} cancelled, tx_hash: {}", inv.invoice_id, tx_hash);
				cancelled += 1;
				last_ok = true;
			},
			Err(e) => {
				println!("  ERROR: {}", e);
				last_ok = false;
			},
		}
	}
	println!("Cancelled {}/{} expired invoices.", cancelled, expired.len());
}
