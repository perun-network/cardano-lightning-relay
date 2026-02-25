//! CardanoOperator trait — abstracts the Cardano operations used by swap/offramp logic.
//!
//! Production code uses `OperatorAgent` (which implements this trait by delegating
//! to its inherent methods). Tests use `MockOperator`.

use cardano_lightning_client::{CardanoError, Invoice, Offramp, OperatorAgent, State};

pub(crate) trait CardanoOperator: Send + Sync {
	fn operator_address(&self) -> &str;
	async fn query_state(&self) -> Result<State, CardanoError>;
	async fn create_invoice(
		&self, amount: i64, owner_pkh: &str, timestamp: i64, expires_at: i64,
	) -> Result<(i64, String), CardanoError>;
	async fn fulfill_invoice(
		&self, invoice: &Invoice, owner_address: &str,
	) -> Result<String, CardanoError>;
	async fn create_offramp(
		&self, amount: i64, payment_hash: &str, refund_address: &str, expires_at: i64,
	) -> Result<(i64, String), CardanoError>;
	async fn fulfill_offramp(&self, offramp: &Offramp) -> Result<String, CardanoError>;
	async fn submit_tx(&self, tx_hex: &str) -> Result<String, CardanoError>;
}

impl CardanoOperator for OperatorAgent {
	fn operator_address(&self) -> &str {
		&self.config().operator_address
	}

	async fn query_state(&self) -> Result<State, CardanoError> {
		self.agent().query_state().await
	}

	async fn create_invoice(
		&self, amount: i64, owner_pkh: &str, timestamp: i64, expires_at: i64,
	) -> Result<(i64, String), CardanoError> {
		self.create_invoice(amount, owner_pkh, timestamp, expires_at).await
	}

	async fn fulfill_invoice(
		&self, invoice: &Invoice, owner_address: &str,
	) -> Result<String, CardanoError> {
		self.fulfill_invoice(invoice, owner_address).await
	}

	async fn create_offramp(
		&self, amount: i64, payment_hash: &str, refund_address: &str, expires_at: i64,
	) -> Result<(i64, String), CardanoError> {
		self.create_offramp(amount, payment_hash, refund_address, expires_at).await
	}

	async fn fulfill_offramp(&self, offramp: &Offramp) -> Result<String, CardanoError> {
		self.fulfill_offramp(offramp).await
	}

	async fn submit_tx(&self, tx_hex: &str) -> Result<String, CardanoError> {
		self.submit_tx(tx_hex).await
	}
}
