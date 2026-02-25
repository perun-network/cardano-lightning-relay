use std::sync::{Arc, Mutex};

use crate::cardano_offramp;
use crate::cardano_ops::CardanoOperator;
use crate::cardano_swap;
use crate::helpers::current_timestamp_ms;
use crate::mapping::{OfframpMapping, OfframpStatus, SwapDb, SwapMapping, SwapStatus};
use cardano_lightning_client::{CardanoError, Invoice, Offramp, State};

// ─── MockOperator ───────────────────────────────────────────────────────────

struct MockOperator {
	address: String,
	invoices: Mutex<Vec<Invoice>>,
	offramps: Mutex<Vec<Offramp>>,
	next_invoice_id: Mutex<i64>,
	next_offramp_id: Mutex<i64>,
	fail_submit: bool,
	fail_fulfill_offramp: bool,
}

impl MockOperator {
	fn new() -> Self {
		MockOperator {
			address: "addr_test1_mock_operator".to_string(),
			invoices: Mutex::new(Vec::new()),
			offramps: Mutex::new(Vec::new()),
			next_invoice_id: Mutex::new(1),
			next_offramp_id: Mutex::new(1),
			fail_submit: false,
			fail_fulfill_offramp: false,
		}
	}

	fn with_fail_submit(mut self) -> Self {
		self.fail_submit = true;
		self
	}

	fn with_fail_fulfill_offramp(mut self) -> Self {
		self.fail_fulfill_offramp = true;
		self
	}

	/// Manually add an invoice to the mock state (for fulfill_swap tests).
	fn add_invoice(&self, invoice: Invoice) {
		self.invoices.lock().unwrap().push(invoice);
	}

	/// Manually add an offramp to the mock state (for complete_offramp tests).
	fn add_offramp(&self, offramp: Offramp) {
		self.offramps.lock().unwrap().push(offramp);
	}
}

impl CardanoOperator for MockOperator {
	fn operator_address(&self) -> &str {
		&self.address
	}

	async fn query_state(&self) -> Result<State, CardanoError> {
		let invoices = self.invoices.lock().unwrap().clone();
		let offramps = self.offramps.lock().unwrap().clone();
		Ok(State {
			total_liquidity: 1_000_000,
			reserved: 0,
			last_invoice_id: invoices.last().map(|i| i.invoice_id).unwrap_or(0),
			invoices,
			last_offramp_id: offramps.last().map(|o| o.offramp_id).unwrap_or(0),
			offramps,
		})
	}

	async fn create_invoice(
		&self, amount: i64, owner_pkh: &str, timestamp: i64, expires_at: i64,
	) -> Result<(i64, String), CardanoError> {
		let mut id_lock = self.next_invoice_id.lock().unwrap();
		let id = *id_lock;
		*id_lock += 1;

		self.invoices.lock().unwrap().push(Invoice {
			invoice_id: id,
			amount,
			owner: owner_pkh.to_string(),
			timestamp,
			expires_at,
		});

		Ok((id, "mock_create_invoice_signed_tx".to_string()))
	}

	async fn fulfill_invoice(
		&self, _invoice: &Invoice, _addr: &str,
	) -> Result<String, CardanoError> {
		Ok("mock_fulfill_invoice_signed_tx".to_string())
	}

	async fn create_offramp(
		&self, amount: i64, payment_hash: &str, refund_address: &str, expires_at: i64,
	) -> Result<(i64, String), CardanoError> {
		let mut id_lock = self.next_offramp_id.lock().unwrap();
		let id = *id_lock;
		*id_lock += 1;

		self.offramps.lock().unwrap().push(Offramp {
			offramp_id: id,
			amount,
			payment_hash: payment_hash.to_string(),
			refund_address: refund_address.to_string(),
			expires_at,
		});

		Ok((id, "mock_create_offramp_signed_tx".to_string()))
	}

	async fn fulfill_offramp(&self, _offramp: &Offramp) -> Result<String, CardanoError> {
		if self.fail_fulfill_offramp {
			return Err(CardanoError::Parse("mock fulfill_offramp failure".to_string()));
		}
		Ok("mock_fulfill_offramp_signed_tx".to_string())
	}

	async fn submit_tx(&self, _tx_hex: &str) -> Result<String, CardanoError> {
		if self.fail_submit {
			return Err(CardanoError::Parse("mock submit_tx failure".to_string()));
		}
		Ok("mock_tx_hash_abc123".to_string())
	}
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn test_db() -> Arc<SwapDb> {
	Arc::new(SwapDb::open(":memory:"))
}

/// Construct a valid bech32 Cardano testnet address for testing.
fn test_cardano_address() -> String {
	use bech32::{ToBase32, Variant};
	let mut data = vec![0x00u8]; // type-0 base address, testnet
	data.extend_from_slice(&[0xab; 28]); // payment key hash
	data.extend_from_slice(&[0xcd; 28]); // stake part
	bech32::encode("addr_test", data.to_base32(), Variant::Bech32).unwrap()
}

fn make_swap(hash: &str, invoice_id: i64, addr: &str) -> SwapMapping {
	let now = current_timestamp_ms();
	SwapMapping {
		payment_hash: hash.to_string(),
		invoice_id,
		amount_cbtc: 100_000,
		cardano_address: addr.to_string(),
		status: SwapStatus::Pending,
		created_at: now,
		expires_at: now + 3_600_000,
		cardano_tx_hash: None,
	}
}

fn make_offramp_mapping(id: i64, hash: &str, status: OfframpStatus) -> OfframpMapping {
	let now = current_timestamp_ms();
	OfframpMapping {
		offramp_id: id,
		bolt11: "lnbc1_test_bolt11".to_string(),
		payment_hash: hash.to_string(),
		amount_cbtc: 50_000,
		cbtc_tx_hash: String::new(),
		status,
		created_at: now,
		lightning_preimage: None,
		deposit_tx_hash: None,
		error_message: None,
		cardano_offramp_tx_hash: None,
		refund_address: "ab".repeat(28),
		expires_at: now + 3_600_000,
	}
}

// ─── Onramp tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_request_swap_happy_path() {
	let mock = MockOperator::new();
	let addr = test_cardano_address();

	let result = cardano_swap::request_swap(&mock, 100_000, &addr).await;
	assert!(result.is_ok(), "request_swap failed: {:?}", result.err());

	let (invoice_id, description) = result.unwrap();
	assert_eq!(invoice_id, 1);
	assert!(description.starts_with("cBTC_SWAP:"));
	assert!(description.contains(&addr));
}

#[tokio::test]
async fn test_request_swap_bad_address() {
	let mock = MockOperator::new();

	let result = cardano_swap::request_swap(&mock, 100_000, "not-a-valid-address!!!").await;
	assert!(result.is_err());
	assert!(result.unwrap_err().contains("invalid bech32"));
}

#[tokio::test(start_paused = true)]
async fn test_fulfill_swap_happy_path() {
	let addr = test_cardano_address();
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	// Pre-populate mock state with an invoice
	mock.add_invoice(Invoice {
		invoice_id: 1,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	// Insert pending swap mapping in DB
	db.insert(&make_swap("payment_hash_001", 1, &addr));

	cardano_swap::fulfill_swap(Arc::clone(&mock), Arc::clone(&db), "payment_hash_001".to_string())
		.await;

	let mapping = db.get_by_payment_hash("payment_hash_001").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
	assert_eq!(mapping.cardano_tx_hash.as_deref(), Some("mock_tx_hash_abc123"));
}

#[tokio::test(start_paused = true)]
async fn test_fulfill_swap_not_in_state() {
	let addr = test_cardano_address();
	let mock = Arc::new(MockOperator::new()); // empty state — no invoices
	let db = test_db();

	db.insert(&make_swap("hash_not_found", 99, &addr));

	cardano_swap::fulfill_swap(Arc::clone(&mock), Arc::clone(&db), "hash_not_found".to_string())
		.await;

	let mapping = db.get_by_payment_hash("hash_not_found").unwrap();
	assert_eq!(mapping.status, SwapStatus::Failed);
}

#[tokio::test(start_paused = true)]
async fn test_fulfill_swap_submit_fails() {
	let addr = test_cardano_address();
	let mock = Arc::new(MockOperator::new().with_fail_submit());
	let db = test_db();

	mock.add_invoice(Invoice {
		invoice_id: 1,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	db.insert(&make_swap("hash_submit_fail", 1, &addr));

	cardano_swap::fulfill_swap(
		Arc::clone(&mock),
		Arc::clone(&db),
		"hash_submit_fail".to_string(),
	)
	.await;

	let mapping = db.get_by_payment_hash("hash_submit_fail").unwrap();
	assert_eq!(mapping.status, SwapStatus::Failed);
}

#[tokio::test]
async fn test_fulfill_swap_skips_non_pending() {
	let addr = test_cardano_address();
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	// Insert a swap that's already Completed
	let mut swap = make_swap("hash_completed", 1, &addr);
	swap.status = SwapStatus::Completed;
	swap.cardano_tx_hash = Some("existing_tx".to_string());
	db.insert(&swap);
	db.update_status("hash_completed", SwapStatus::Completed, Some("existing_tx"));

	cardano_swap::fulfill_swap(
		Arc::clone(&mock),
		Arc::clone(&db),
		"hash_completed".to_string(),
	)
	.await;

	// Status should remain Completed — fulfill_swap returns early
	let mapping = db.get_by_payment_hash("hash_completed").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
	assert_eq!(mapping.cardano_tx_hash.as_deref(), Some("existing_tx"));
}

// ─── Offramp tests ──────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn test_complete_offramp_happy_path() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	// Pre-populate mock state with an offramp entry
	mock.add_offramp(Offramp {
		offramp_id: 1,
		amount: 50_000,
		payment_hash: "off_hash_001".to_string(),
		refund_address: "ab".repeat(28),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	// Insert offramp mapping in PayingLightning status (as if Lightning payment was just sent)
	let mut mapping = make_offramp_mapping(1, "off_hash_001", OfframpStatus::PayingLightning);
	mapping.lightning_preimage = None;
	db.insert_offramp(&mapping);

	cardano_offramp::complete_offramp(
		Arc::clone(&mock),
		Arc::clone(&db),
		"off_hash_001".to_string(),
		"preimage_hex_001".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("off_hash_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Completed);
	assert_eq!(result.lightning_preimage.as_deref(), Some("preimage_hex_001"));
	assert_eq!(result.deposit_tx_hash.as_deref(), Some("mock_tx_hash_abc123"));
}

#[tokio::test(start_paused = true)]
async fn test_complete_offramp_fulfill_fails() {
	let mock = Arc::new(MockOperator::new().with_fail_fulfill_offramp());
	let db = test_db();

	mock.add_offramp(Offramp {
		offramp_id: 1,
		amount: 50_000,
		payment_hash: "off_fail_001".to_string(),
		refund_address: "ab".repeat(28),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	db.insert_offramp(&make_offramp_mapping(1, "off_fail_001", OfframpStatus::PayingLightning));

	cardano_offramp::complete_offramp(
		Arc::clone(&mock),
		Arc::clone(&db),
		"off_fail_001".to_string(),
		"preimage_hex".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("off_fail_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	assert!(result.error_message.as_deref().unwrap().contains("fulfill_offramp"));
}

#[tokio::test]
async fn test_complete_offramp_skips_non_paying() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	// Insert offramp in AwaitingDeposit status — should be skipped
	db.insert_offramp(&make_offramp_mapping(
		1,
		"off_skip_001",
		OfframpStatus::AwaitingDeposit,
	));

	cardano_offramp::complete_offramp(
		Arc::clone(&mock),
		Arc::clone(&db),
		"off_skip_001".to_string(),
		"preimage_hex".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("off_skip_001").unwrap();
	assert_eq!(result.status, OfframpStatus::AwaitingDeposit);
}

#[tokio::test]
async fn test_handle_offramp_payment_failed() {
	let db = test_db();

	// Insert offramp in PayingLightning status
	db.insert_offramp(&make_offramp_mapping(
		1,
		"off_pay_fail_001",
		OfframpStatus::PayingLightning,
	));

	cardano_offramp::handle_offramp_payment_failed(
		Arc::clone(&db),
		"off_pay_fail_001".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("off_pay_fail_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	assert!(result.error_message.is_some());
	assert!(result.error_message.as_deref().unwrap().contains("Lightning payment failed"));
}

// ─── End-to-end pipeline tests ──────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn test_onramp_end_to_end() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();
	let addr = test_cardano_address();

	// Step 1: request_swap creates the on-chain invoice
	let (invoice_id, description) =
		cardano_swap::request_swap(&*mock, 100_000, &addr).await.unwrap();

	// Step 2: store_swap_mapping (simulating what the API handler does after creating BOLT11)
	let now = current_timestamp_ms();
	cardano_swap::store_swap_mapping(&db, "e2e_hash_001", invoice_id, 100_000, &addr, now + 3_600_000);

	// Verify mapping is stored as Pending
	let mapping = db.get_by_payment_hash("e2e_hash_001").unwrap();
	assert_eq!(mapping.status, SwapStatus::Pending);
	assert_eq!(mapping.invoice_id, invoice_id);

	// Step 3: fulfill_swap (simulating PaymentClaimed event)
	cardano_swap::fulfill_swap(
		Arc::clone(&mock),
		Arc::clone(&db),
		"e2e_hash_001".to_string(),
	)
	.await;

	// Verify final state
	let mapping = db.get_by_payment_hash("e2e_hash_001").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
	assert!(mapping.cardano_tx_hash.is_some());
	assert!(description.starts_with("cBTC_SWAP:"));
}

#[tokio::test(start_paused = true)]
async fn test_offramp_end_to_end() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	// Pre-populate mock with an offramp entry (simulating CreateOfframp TX already confirmed)
	mock.add_offramp(Offramp {
		offramp_id: 1,
		amount: 50_000,
		payment_hash: "e2e_off_hash".to_string(),
		refund_address: "ab".repeat(28),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	// Insert offramp mapping in PayingLightning (simulating post-deposit, post-Lightning-send)
	db.insert_offramp(&make_offramp_mapping(1, "e2e_off_hash", OfframpStatus::PayingLightning));

	// complete_offramp (simulating PaymentSent event)
	cardano_offramp::complete_offramp(
		Arc::clone(&mock),
		Arc::clone(&db),
		"e2e_off_hash".to_string(),
		"e2e_preimage".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("e2e_off_hash").unwrap();
	assert_eq!(result.status, OfframpStatus::Completed);
	assert_eq!(result.lightning_preimage.as_deref(), Some("e2e_preimage"));
	assert!(result.deposit_tx_hash.is_some());
}

#[tokio::test(start_paused = true)]
async fn test_multiple_concurrent_swaps() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();
	let addr = test_cardano_address();

	// Create 3 onramp swaps
	for i in 1..=3 {
		let (invoice_id, _desc) =
			cardano_swap::request_swap(&*mock, 100_000 * i, &addr).await.unwrap();
		let hash = format!("multi_hash_{}", i);
		let now = current_timestamp_ms();
		cardano_swap::store_swap_mapping(&db, &hash, invoice_id, 100_000 * i, &addr, now + 3_600_000);
	}

	// Create 2 offramp mappings
	for i in 1..=2 {
		let hash = format!("multi_off_{}", i);
		mock.add_offramp(Offramp {
			offramp_id: i,
			amount: 50_000 * i,
			payment_hash: hash.clone(),
			refund_address: "ab".repeat(28),
			expires_at: current_timestamp_ms() + 3_600_000,
		});
		db.insert_offramp(&make_offramp_mapping(i, &hash, OfframpStatus::PayingLightning));
	}

	// Fulfill all 3 onramps
	for i in 1..=3 {
		let hash = format!("multi_hash_{}", i);
		cardano_swap::fulfill_swap(Arc::clone(&mock), Arc::clone(&db), hash).await;
	}

	// Complete both offramps
	for i in 1..=2 {
		let hash = format!("multi_off_{}", i);
		cardano_offramp::complete_offramp(
			Arc::clone(&mock),
			Arc::clone(&db),
			hash,
			format!("preimage_{}", i),
		)
		.await;
	}

	// Verify all onramps completed
	for i in 1..=3 {
		let hash = format!("multi_hash_{}", i);
		let m = db.get_by_payment_hash(&hash).unwrap();
		assert_eq!(m.status, SwapStatus::Completed, "onramp {} should be Completed", i);
	}

	// Verify all offramps completed
	for i in 1..=2 {
		let hash = format!("multi_off_{}", i);
		let m = db.get_offramp_by_payment_hash(&hash).unwrap();
		assert_eq!(m.status, OfframpStatus::Completed, "offramp {} should be Completed", i);
	}
}
