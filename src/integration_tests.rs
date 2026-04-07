use std::sync::{Arc, Mutex};

use crate::cardano_offramp;
use crate::cardano_ops::CardanoOperator;
use crate::cardano_swap;
use crate::helpers::current_timestamp_ms;
use crate::mapping::{OfframpMapping, OfframpStatus, SwapDb, SwapMapping, SwapStatus};
use crate::recovery;
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
	fail_query_state: bool,
	fail_send_cbtc: bool,
	fail_cancel_offramp: bool,
	/// Delay before returning from query_state (creates concurrency windows for race tests).
	query_delay: Option<std::time::Duration>,
	/// Tracks how many times submit_tx was called (for concurrency assertions).
	submit_count: Mutex<u32>,
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
			fail_query_state: false,
			fail_send_cbtc: false,
			fail_cancel_offramp: false,
			query_delay: None,
			submit_count: Mutex::new(0),
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

	#[allow(dead_code)]
	fn with_fail_query_state(mut self) -> Self {
		self.fail_query_state = true;
		self
	}

	fn with_fail_send_cbtc(mut self) -> Self {
		self.fail_send_cbtc = true;
		self
	}

	fn with_fail_cancel_offramp(mut self) -> Self {
		self.fail_cancel_offramp = true;
		self
	}

	fn with_query_delay(mut self, delay: std::time::Duration) -> Self {
		self.query_delay = Some(delay);
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

	/// Remove an invoice (simulate already-fulfilled on-chain).
	#[allow(dead_code)]
	fn remove_invoice(&self, invoice_id: i64) {
		self.invoices.lock().unwrap().retain(|i| i.invoice_id != invoice_id);
	}

	/// Remove an offramp (simulate already-fulfilled on-chain).
	#[allow(dead_code)]
	fn remove_offramp(&self, offramp_id: i64) {
		self.offramps.lock().unwrap().retain(|o| o.offramp_id != offramp_id);
	}
}

impl CardanoOperator for MockOperator {
	fn operator_address(&self) -> &str {
		&self.address
	}

	async fn query_state(&self) -> Result<State, CardanoError> {
		if self.fail_query_state {
			return Err(CardanoError::Parse("mock query_state failure".to_string()));
		}
		if let Some(delay) = self.query_delay {
			tokio::time::sleep(delay).await;
		}
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

	async fn cancel_offramp(&self, _offramp_id: i64) -> Result<String, CardanoError> {
		if self.fail_cancel_offramp {
			return Err(CardanoError::Parse("mock cancel_offramp failure".to_string()));
		}
		Ok("mock_cancel_offramp_signed_tx".to_string())
	}

	async fn send_cbtc(&self, _target: &str, _amount: i64) -> Result<String, CardanoError> {
		if self.fail_send_cbtc {
			return Err(CardanoError::Parse("mock send_cbtc failure".to_string()));
		}
		Ok("mock_send_cbtc_signed_tx".to_string())
	}

	async fn submit_tx(&self, _tx_hex: &str) -> Result<String, CardanoError> {
		if self.fail_submit {
			return Err(CardanoError::Parse("mock submit_tx failure".to_string()));
		}
		*self.submit_count.lock().unwrap() += 1;
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
		create_tx_hash: None,
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

	let result = cardano_swap::request_swap(&mock, 100_000, &addr, 3_600_000).await;
	assert!(result.is_ok(), "request_swap failed: {:?}", result.err());

	let (invoice_id, description, create_tx) = result.unwrap();
	assert_eq!(invoice_id, 1);
	assert!(description.starts_with("cBTC_SWAP:"));
	assert!(description.contains(&addr));
	assert_eq!(create_tx, "mock_tx_hash_abc123");
}

#[tokio::test]
async fn test_request_swap_bad_address() {
	let mock = MockOperator::new();

	let result = cardano_swap::request_swap(&mock, 100_000, "not-a-valid-address!!!", 3_600_000).await;
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
	let operator = Arc::new(MockOperator::new());

	// Insert offramp in PayingLightning status
	db.insert_offramp(&make_offramp_mapping(
		1,
		"off_pay_fail_001",
		OfframpStatus::PayingLightning,
	));

	cardano_offramp::handle_offramp_payment_failed(
		operator,
		Arc::clone(&db),
		"off_pay_fail_001".to_string(),
	)
	.await;

	let result = db.get_offramp_by_payment_hash("off_pay_fail_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	assert!(result.error_message.is_some());
}

// ─── End-to-end pipeline tests ──────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn test_onramp_end_to_end() {
	let mock = Arc::new(MockOperator::new());
	let db = test_db();
	let addr = test_cardano_address();

	// Step 1: request_swap creates the on-chain invoice
	let (invoice_id, description, create_tx) =
		cardano_swap::request_swap(&*mock, 100_000, &addr, 3_600_000).await.unwrap();

	// Step 2: store_swap_mapping (simulating what the API handler does after creating BOLT11)
	let now = current_timestamp_ms();
	cardano_swap::store_swap_mapping(&db, "e2e_hash_001", invoice_id, 100_000, &addr, now + 3_600_000, &create_tx);

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
		let (invoice_id, _desc, create_tx) =
			cardano_swap::request_swap(&*mock, 100_000 * i, &addr, 3_600_000).await.unwrap();
		let hash = format!("multi_hash_{}", i);
		let now = current_timestamp_ms();
		cardano_swap::store_swap_mapping(&db, &hash, invoice_id, 100_000 * i, &addr, now + 3_600_000, &create_tx);
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

// ─── Recovery tests ────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn test_recover_fulfilling_swap_already_fulfilled_on_chain() {
	// Invoice NOT on-chain (already fulfilled) → recovery should mark Completed
	let mock = MockOperator::new(); // empty state = invoice not found
	let db = test_db();
	let addr = test_cardano_address();

	let mut swap = make_swap("recovery_hash_001", 1, &addr);
	swap.status = SwapStatus::Fulfilling;
	db.insert(&swap);
	db.update_status("recovery_hash_001", SwapStatus::Fulfilling, None);

	recovery::recover_fulfilling_swaps(&mock, &db).await;

	let mapping = db.get_by_payment_hash("recovery_hash_001").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
}

#[tokio::test(start_paused = true)]
async fn test_recover_fulfilling_swap_retry_succeeds() {
	// Invoice still on-chain → recovery retries FulfillInvoice TX
	let mock = MockOperator::new();
	let db = test_db();
	let addr = test_cardano_address();

	mock.add_invoice(Invoice {
		invoice_id: 5,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	let mut swap = make_swap("recovery_hash_002", 5, &addr);
	swap.status = SwapStatus::Fulfilling;
	db.insert(&swap);
	db.update_status("recovery_hash_002", SwapStatus::Fulfilling, None);

	recovery::recover_fulfilling_swaps(&mock, &db).await;

	let mapping = db.get_by_payment_hash("recovery_hash_002").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
	assert_eq!(mapping.cardano_tx_hash.as_deref(), Some("mock_tx_hash_abc123"));
}

#[tokio::test(start_paused = true)]
async fn test_recover_fulfilling_swap_submit_fails_stays_fulfilling() {
	// Invoice on-chain but submit fails → swap stays in Fulfilling (not marked Failed)
	let mock = MockOperator::new().with_fail_submit();
	let db = test_db();
	let addr = test_cardano_address();

	mock.add_invoice(Invoice {
		invoice_id: 6,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	let mut swap = make_swap("recovery_hash_003", 6, &addr);
	swap.status = SwapStatus::Fulfilling;
	db.insert(&swap);
	db.update_status("recovery_hash_003", SwapStatus::Fulfilling, None);

	recovery::recover_fulfilling_swaps(&mock, &db).await;

	// Should remain Fulfilling — recovery doesn't mark Failed on retry failure
	// (so next startup can retry again)
	let mapping = db.get_by_payment_hash("recovery_hash_003").unwrap();
	assert_eq!(mapping.status, SwapStatus::Fulfilling);
}

#[tokio::test(start_paused = true)]
async fn test_recover_depositing_offramp_already_fulfilled() {
	// Offramp NOT on-chain (already fulfilled) → mark Completed
	let mock = MockOperator::new(); // empty state
	let db = test_db();

	let mapping = make_offramp_mapping(1, "rec_off_001", OfframpStatus::DepositingToPool);
	db.insert_offramp(&mapping);
	db.transition_offramp_status(1, OfframpStatus::AwaitingDeposit, OfframpStatus::DepositingToPool);

	recovery::recover_depositing_offramps(&mock, &db).await;

	let result = db.get_offramp_by_payment_hash("rec_off_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Completed);
}

#[tokio::test(start_paused = true)]
async fn test_recover_depositing_offramp_retry_succeeds() {
	// Offramp still on-chain → retry FulfillOfframp TX
	let mock = MockOperator::new();
	let db = test_db();

	mock.add_offramp(Offramp {
		offramp_id: 2,
		amount: 50_000,
		payment_hash: "rec_off_002".to_string(),
		refund_address: "ab".repeat(28),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	let mapping = make_offramp_mapping(2, "rec_off_002", OfframpStatus::DepositingToPool);
	db.insert_offramp(&mapping);
	db.transition_offramp_status(2, OfframpStatus::AwaitingDeposit, OfframpStatus::DepositingToPool);

	recovery::recover_depositing_offramps(&mock, &db).await;

	let result = db.get_offramp_by_payment_hash("rec_off_002").unwrap();
	assert_eq!(result.status, OfframpStatus::Completed);
}

#[tokio::test(start_paused = true)]
async fn test_recover_paying_lightning_marks_failed() {
	// Stuck in PayingLightning → recovery marks Failed (can't retry Lightning payment)
	let mock = MockOperator::new();
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(
		3, "rec_off_003", OfframpStatus::PayingLightning,
	));
	// Need to transition to PayingLightning via the expected path
	db.transition_offramp_status(3, OfframpStatus::AwaitingDeposit, OfframpStatus::PayingLightning);

	recovery::recover_depositing_offramps(&mock, &db).await;

	let result = db.get_offramp_by_payment_hash("rec_off_003").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	assert!(result.error_message.as_deref().unwrap().contains("crashed"));
}

// ─── Race condition / duplicate rejection tests ────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_fulfill_swap_atomic_transition_prevents_double_fulfill() {
	// Two concurrent fulfill_swap calls on real threads — the query_delay creates a window
	// where both tasks are past the DB lookup but racing on transition_status.
	// Only one should win the transition and submit the TX.
	let addr = test_cardano_address();
	let mock = Arc::new(
		MockOperator::new().with_query_delay(std::time::Duration::from_millis(50)),
	);
	let db = test_db();

	mock.add_invoice(Invoice {
		invoice_id: 10,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	db.insert(&make_swap("double_fulfill_hash", 10, &addr));

	// Spawn both on real threads so they actually race
	let (h1, h2) = {
		let m1 = Arc::clone(&mock);
		let m2 = Arc::clone(&mock);
		let d1 = Arc::clone(&db);
		let d2 = Arc::clone(&db);
		(
			tokio::spawn(async move {
				cardano_swap::fulfill_swap(m1, d1, "double_fulfill_hash".to_string()).await;
			}),
			tokio::spawn(async move {
				cardano_swap::fulfill_swap(m2, d2, "double_fulfill_hash".to_string()).await;
			}),
		)
	};
	h1.await.unwrap();
	h2.await.unwrap();

	let mapping = db.get_by_payment_hash("double_fulfill_hash").unwrap();
	assert_eq!(mapping.status, SwapStatus::Completed);
	// Only one task should have submitted a TX (the one that won the transition)
	assert_eq!(*mock.submit_count.lock().unwrap(), 1, "expected exactly 1 submit_tx call");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_complete_offramp_atomic_transition_prevents_double_complete() {
	let mock = Arc::new(
		MockOperator::new().with_query_delay(std::time::Duration::from_millis(50)),
	);
	let db = test_db();

	mock.add_offramp(Offramp {
		offramp_id: 10,
		amount: 50_000,
		payment_hash: "double_complete_hash".to_string(),
		refund_address: "ab".repeat(28),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	db.insert_offramp(&make_offramp_mapping(10, "double_complete_hash", OfframpStatus::PayingLightning));

	let (h1, h2) = {
		let m1 = Arc::clone(&mock);
		let m2 = Arc::clone(&mock);
		let d1 = Arc::clone(&db);
		let d2 = Arc::clone(&db);
		(
			tokio::spawn(async move {
				cardano_offramp::complete_offramp(m1, d1, "double_complete_hash".to_string(), "preimage_a".to_string()).await;
			}),
			tokio::spawn(async move {
				cardano_offramp::complete_offramp(m2, d2, "double_complete_hash".to_string(), "preimage_b".to_string()).await;
			}),
		)
	};
	h1.await.unwrap();
	h2.await.unwrap();

	let result = db.get_offramp_by_payment_hash("double_complete_hash").unwrap();
	assert_eq!(result.status, OfframpStatus::Completed);
	// Only one task should have submitted FulfillOfframp (the one that won the transition)
	assert_eq!(*mock.submit_count.lock().unwrap(), 1, "expected exactly 1 submit_tx call");
}

// ─── Duplicate rejection tests (via actual request_offramp) ────────────────

/// Build a valid regtest BOLT11 invoice for testing.
/// Uses a deterministic key so the invoice string is reproducible.
fn build_test_bolt11(amount_msat: u64, payment_hash_bytes: &[u8; 32]) -> String {
	use bitcoin::hashes::{sha256, Hash};
	use bitcoin::secp256k1::{Secp256k1, SecretKey};
	use lightning_invoice::{Currency, InvoiceBuilder};
	use lightning::types::payment::PaymentSecret;

	let secret_key = SecretKey::from_slice(&[0x42; 32]).unwrap();
	let payment_hash = sha256::Hash::from_slice(payment_hash_bytes).unwrap();
	let payment_secret = PaymentSecret([0x01; 32]);

	let invoice = InvoiceBuilder::new(Currency::Regtest)
		.description("test invoice".into())
		.payment_hash(payment_hash)
		.payment_secret(payment_secret)
		.amount_milli_satoshis(amount_msat)
		.timestamp(std::time::SystemTime::now())
		.min_final_cltv_expiry_delta(144)
		.build_signed(|hash| {
			Secp256k1::new().sign_ecdsa_recoverable(hash, &secret_key)
		})
		.unwrap();

	invoice.to_string()
}

#[tokio::test]
async fn test_request_offramp_rejects_duplicate_bolt11_invoice() {
	let mock = MockOperator::new();
	let db = Arc::new(SwapDb::open(":memory:"));
	let addr = test_cardano_address();

	let bolt11 = build_test_bolt11(50_000, &[0xAA; 32]);

	// First request succeeds
	let result1 = cardano_offramp::request_offramp(&mock, &db, &bolt11, 50_000, &addr, 3_600_000).await;
	assert!(result1.is_ok(), "first request should succeed: {:?}", result1.err());

	// Second request with same BOLT11 is rejected
	let result2 = cardano_offramp::request_offramp(&mock, &db, &bolt11, 50_000, &addr, 3_600_000).await;
	assert!(result2.is_err());
	let err = result2.unwrap_err();
	assert!(err.contains("already used"), "expected 'already used' error, got: {}", err);
}

#[tokio::test]
async fn test_request_offramp_rejects_amount_mismatch() {
	let mock = MockOperator::new();
	let db = Arc::new(SwapDb::open(":memory:"));
	let addr = test_cardano_address();

	// Invoice is for 50_000 msat, request claims 99_999 cBTC
	let bolt11 = build_test_bolt11(50_000, &[0xBB; 32]);

	let result = cardano_offramp::request_offramp(&mock, &db, &bolt11, 99_999, &addr, 3_600_000).await;
	assert!(result.is_err());
	let err = result.unwrap_err();
	assert!(err.contains("does not match"), "expected amount mismatch error, got: {}", err);
}

#[tokio::test]
async fn test_request_offramp_rejects_mainnet_invoice_on_testnet_relay() {
	let mock = MockOperator::new(); // address starts with addr_test = testnet relay
	let db = Arc::new(SwapDb::open(":memory:"));
	let addr = test_cardano_address();

	// Build a mainnet invoice (Currency::Bitcoin)
	let bolt11 = {
		use bitcoin::hashes::{sha256, Hash};
		use bitcoin::secp256k1::{Secp256k1, SecretKey};
		use lightning_invoice::{Currency, InvoiceBuilder};
		use lightning::types::payment::PaymentSecret;

		let sk = SecretKey::from_slice(&[0x42; 32]).unwrap();
		let invoice = InvoiceBuilder::new(Currency::Bitcoin)
			.description("mainnet invoice".into())
			.payment_hash(sha256::Hash::from_slice(&[0xCC; 32]).unwrap())
			.payment_secret(PaymentSecret([0x01; 32]))
			.amount_milli_satoshis(50_000)
			.timestamp(std::time::SystemTime::now())
			.min_final_cltv_expiry_delta(144)
			.build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &sk))
			.unwrap();
		invoice.to_string()
	};

	let result = cardano_offramp::request_offramp(&mock, &db, &bolt11, 50_000, &addr, 3_600_000).await;
	assert!(result.is_err());
	let err = result.unwrap_err();
	assert!(err.contains("network"), "expected network mismatch error, got: {}", err);
}

#[tokio::test]
async fn test_request_offramp_happy_path() {
	let mock = MockOperator::new();
	let db = Arc::new(SwapDb::open(":memory:"));
	let addr = test_cardano_address();

	let bolt11 = build_test_bolt11(50_000, &[0xDD; 32]);

	let result = cardano_offramp::request_offramp(&mock, &db, &bolt11, 50_000, &addr, 3_600_000).await;
	assert!(result.is_ok(), "request_offramp failed: {:?}", result.err());

	let (offramp_id, operator_address, payment_hash) = result.unwrap();
	assert_eq!(offramp_id, 1);
	assert_eq!(operator_address, "addr_test1_mock_operator");
	assert!(!payment_hash.is_empty());

	// Verify DB entry was created
	let mapping = db.get_offramp_by_id(1).unwrap();
	assert_eq!(mapping.status, OfframpStatus::AwaitingDeposit);
	assert_eq!(mapping.amount_cbtc, 50_000);
}

// ─── Refund and expiry flow tests ──────────────────────────────────────────

#[tokio::test]
async fn test_offramp_payment_failed_refund_and_cancel_succeed() {
	// Both refund and cancel succeed — message preserves both refund and cancel info
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(1, "refund_hash_001", OfframpStatus::PayingLightning));

	cardano_offramp::handle_offramp_payment_failed(
		Arc::clone(&mock), Arc::clone(&db), "refund_hash_001".to_string(),
	).await;

	let result = db.get_offramp_by_payment_hash("refund_hash_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	let msg = result.error_message.as_deref().unwrap();
	assert!(msg.contains("refunded"), "expected refund info, got: {}", msg);
	assert!(msg.contains("cancelled on-chain"), "expected cancel info, got: {}", msg);
}

#[tokio::test]
async fn test_offramp_payment_failed_refund_fails_cancel_fails() {
	// Both refund and cancel fail — message should reflect refund failure
	let mock = Arc::new(MockOperator::new().with_fail_send_cbtc().with_fail_cancel_offramp());
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(1, "refund_fail_001", OfframpStatus::PayingLightning));

	cardano_offramp::handle_offramp_payment_failed(
		Arc::clone(&mock), Arc::clone(&db), "refund_fail_001".to_string(),
	).await;

	let result = db.get_offramp_by_payment_hash("refund_fail_001").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	let msg = result.error_message.as_deref().unwrap();
	// Cancel also fails so it doesn't overwrite — refund build failure message preserved
	assert!(msg.contains("refund") && msg.contains("failed"), "got: {}", msg);
}

#[tokio::test]
async fn test_offramp_payment_failed_no_refund_address() {
	// No refund address, cancel also fails — "no refund address" message preserved
	let mock = Arc::new(MockOperator::new().with_fail_cancel_offramp());
	let db = test_db();

	let mut mapping = make_offramp_mapping(1, "no_refund_addr", OfframpStatus::PayingLightning);
	mapping.refund_address = String::new();
	db.insert_offramp(&mapping);

	cardano_offramp::handle_offramp_payment_failed(
		Arc::clone(&mock), Arc::clone(&db), "no_refund_addr".to_string(),
	).await;

	let result = db.get_offramp_by_payment_hash("no_refund_addr").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	let msg = result.error_message.as_deref().unwrap();
	assert!(msg.contains("no refund address"), "got: {}", msg);
}

#[tokio::test]
async fn test_offramp_payment_failed_refund_ok_cancel_fails() {
	// Refund succeeds but cancel fails — refund message preserved
	let mock = Arc::new(MockOperator::new().with_fail_cancel_offramp());
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(1, "refund_ok_cancel_fail", OfframpStatus::PayingLightning));

	cardano_offramp::handle_offramp_payment_failed(
		Arc::clone(&mock), Arc::clone(&db), "refund_ok_cancel_fail".to_string(),
	).await;

	let result = db.get_offramp_by_payment_hash("refund_ok_cancel_fail").unwrap();
	assert_eq!(result.status, OfframpStatus::Failed);
	let msg = result.error_message.as_deref().unwrap();
	assert!(msg.contains("refunded"), "got: {}", msg);
}

// ─── Expiry and active cap tests (via actual swap/offramp functions) ───────

#[tokio::test(start_paused = true)]
async fn test_fulfill_swap_rejects_expired_invoice() {
	// A swap whose expires_at has passed should be marked Failed, not fulfilled
	let addr = test_cardano_address();
	let mock = Arc::new(MockOperator::new());
	let db = test_db();

	mock.add_invoice(Invoice {
		invoice_id: 20,
		amount: 100_000,
		owner: "ab".repeat(28),
		timestamp: current_timestamp_ms(),
		expires_at: current_timestamp_ms() + 3_600_000,
	});

	// Insert swap with expires_at in the past
	let mut swap = make_swap("expired_swap", 20, &addr);
	swap.expires_at = current_timestamp_ms() - 1000;
	db.insert(&swap);

	cardano_swap::fulfill_swap(Arc::clone(&mock), Arc::clone(&db), "expired_swap".to_string()).await;

	let mapping = db.get_by_payment_hash("expired_swap").unwrap();
	assert_eq!(mapping.status, SwapStatus::Failed);
	// submit_tx should NOT have been called
	assert_eq!(*mock.submit_count.lock().unwrap(), 0);
}

#[tokio::test]
async fn test_request_offramp_fills_active_cap() {
	// Fill the DB with active offramps, then verify the cap is enforceable
	let mock = MockOperator::new();
	let db = Arc::new(SwapDb::open(":memory:"));
	let addr = test_cardano_address();

	// Create 3 offramps via actual request_offramp (each needs a unique BOLT11)
	for i in 0..3u8 {
		let mut hash_bytes = [0u8; 32];
		hash_bytes[0] = i + 0xE0;
		let bolt11 = build_test_bolt11(50_000, &hash_bytes);
		let result = cardano_offramp::request_offramp(&mock, &db, &bolt11, 50_000, &addr, 3_600_000).await;
		assert!(result.is_ok(), "offramp {} should succeed: {:?}", i, result.err());
	}

	// All 3 should be in AwaitingDeposit = active
	assert_eq!(db.count_active_offramps(), 3);

	// Complete one → active count drops
	db.update_offramp_status(1, OfframpStatus::Completed, None, None, None);
	assert_eq!(db.count_active_offramps(), 2);
}

#[tokio::test]
async fn test_cancel_offramp_on_chain_success() {
	let mock = MockOperator::new();
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(1, "cancel_hash", OfframpStatus::Failed));

	let mapping = db.get_offramp_by_id(1).unwrap();
	cardano_offramp::cancel_offramp_on_chain(&mock, &db, &mapping).await;

	let result = db.get_offramp_by_id(1).unwrap();
	let msg = result.error_message.as_deref().unwrap_or("");
	assert!(msg.contains("cancelled on-chain"), "got: {}", msg);
}

#[tokio::test]
async fn test_cancel_offramp_on_chain_fails_gracefully() {
	let mock = MockOperator::new().with_fail_cancel_offramp();
	let db = test_db();

	db.insert_offramp(&make_offramp_mapping(1, "cancel_fail_hash", OfframpStatus::Failed));

	let mapping = db.get_offramp_by_id(1).unwrap();
	cardano_offramp::cancel_offramp_on_chain(&mock, &db, &mapping).await;

	// Should not panic, error message should not change since cancel_offramp build failed
	let result = db.get_offramp_by_id(1).unwrap();
	// No "cancelled on-chain" message
	let msg = result.error_message.as_deref().unwrap_or("");
	assert!(!msg.contains("cancelled on-chain"), "should not have cancelled, got: {}", msg);
}
