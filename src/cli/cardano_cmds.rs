//! CLI commands for Cardano LM contract operations.

use cardano_lightning_client::OperatorAgent;
use std::sync::Arc;

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
	let signed_tx = match operator.deposit(amount).await {
		Ok(tx) => tx,
		Err(e) => {
			println!("ERROR: failed to build deposit tx: {}", e);
			return;
		},
	};

	println!("Submitting tx...");
	match operator.submit_tx(&signed_tx).await {
		Ok(tx_hash) => println!("SUCCESS: deposit submitted, tx_hash: {}", tx_hash),
		Err(e) => println!("ERROR: failed to submit deposit tx: {}", e),
	}
}

/// Withdraw cBTC from the pool.
pub(crate) async fn cardano_withdraw(operator: &Arc<OperatorAgent>, amount: i64) {
	println!("Building withdraw tx for {} cBTC units...", amount);
	let signed_tx = match operator.withdraw(amount).await {
		Ok(tx) => tx,
		Err(e) => {
			println!("ERROR: failed to build withdraw tx: {}", e);
			return;
		},
	};

	println!("Submitting tx...");
	match operator.submit_tx(&signed_tx).await {
		Ok(tx_hash) => println!("SUCCESS: withdraw submitted, tx_hash: {}", tx_hash),
		Err(e) => println!("ERROR: failed to submit withdraw tx: {}", e),
	}
}

/// Cancel expired invoices in the pool.
pub(crate) async fn cancel_expired(operator: &Arc<OperatorAgent>) {
	let state = match operator.agent().query_state().await {
		Ok(s) => s,
		Err(e) => {
			println!("ERROR: failed to query pool state: {}", e);
			return;
		},
	};

	let now_ms = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64;

	let expired: Vec<_> = state.invoices.iter().filter(|i| i.expires_at < now_ms).collect();

	if expired.is_empty() {
		println!("No expired invoices found.");
		return;
	}

	println!("Found {} expired invoice(s), cancelling...", expired.len());
	for inv in &expired {
		println!("  Cancelling invoice #{} (expired at {})...", inv.invoice_id, inv.expires_at);
		let signed_tx = match operator.cancel_invoice(inv.invoice_id).await {
			Ok(tx) => tx,
			Err(e) => {
				println!("  ERROR: failed to build cancel tx for invoice #{}: {}", inv.invoice_id, e);
				continue;
			},
		};
		match operator.submit_tx(&signed_tx).await {
			Ok(tx_hash) => println!("  SUCCESS: invoice #{} cancelled, tx_hash: {}", inv.invoice_id, tx_hash),
			Err(e) => println!("  ERROR: failed to submit cancel tx for invoice #{}: {}", inv.invoice_id, e),
		}
	}
}
