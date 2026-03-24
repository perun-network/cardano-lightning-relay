//! SwapDb — SQLite-backed swap mapping storage.
//!
//! Tracks the lifecycle of BTC→cBTC swaps (onramp) and cBTC→BTC swaps (offramp)
//! from invoice creation through Lightning payment to Cardano fulfillment/deposit.

use rusqlite::{Connection, params};
use std::sync::Mutex;

// ─── Onramp types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SwapStatus {
	/// LM invoice created, waiting for Lightning payment.
	Pending,
	/// Lightning payment received, Cardano fulfillment in progress.
	Fulfilling,
	/// cBTC sent to user on Cardano. Swap complete.
	Completed,
	/// Swap failed (Cardano tx failed after Lightning payment).
	Failed,
	/// Invoice expired and cancelled on-chain.
	Expired,
}

impl SwapStatus {
	fn as_str(&self) -> &'static str {
		match self {
			SwapStatus::Pending => "pending",
			SwapStatus::Fulfilling => "fulfilling",
			SwapStatus::Completed => "completed",
			SwapStatus::Failed => "failed",
			SwapStatus::Expired => "expired",
		}
	}

	fn from_str(s: &str) -> Self {
		match s {
			"pending" => SwapStatus::Pending,
			"fulfilling" => SwapStatus::Fulfilling,
			"completed" => SwapStatus::Completed,
			"failed" => SwapStatus::Failed,
			"expired" => SwapStatus::Expired,
			_ => SwapStatus::Failed,
		}
	}
}

#[derive(Debug, Clone)]
pub(crate) struct SwapMapping {
	pub payment_hash: String,
	pub invoice_id: i64,
	pub amount_cbtc: i64,
	pub cardano_address: String,
	pub status: SwapStatus,
	pub created_at: i64,
	pub expires_at: i64,
	pub cardano_tx_hash: Option<String>,
	pub create_tx_hash: Option<String>,
}

pub(crate) struct SwapDb {
	conn: Mutex<Connection>,
}

impl SwapDb {
	pub fn open(path: &str) -> Self {
		let conn = Connection::open(path).expect("failed to open swap database");
		conn.execute_batch(
			"CREATE TABLE IF NOT EXISTS swap_mappings (
				payment_hash   TEXT PRIMARY KEY,
				invoice_id     INTEGER NOT NULL,
				amount_cbtc    INTEGER NOT NULL,
				cardano_address TEXT NOT NULL,
				status         TEXT NOT NULL DEFAULT 'pending',
				created_at     INTEGER NOT NULL,
				expires_at     INTEGER NOT NULL,
				cardano_tx_hash TEXT,
				create_tx_hash TEXT
			);
			CREATE TABLE IF NOT EXISTS offramp_mappings (
				offramp_id             INTEGER PRIMARY KEY,
				bolt11                 TEXT NOT NULL,
				payment_hash           TEXT NOT NULL,
				amount_cbtc            INTEGER NOT NULL,
				cbtc_tx_hash           TEXT NOT NULL DEFAULT '',
				status                 TEXT NOT NULL DEFAULT 'awaiting_deposit',
				created_at             INTEGER NOT NULL,
				lightning_preimage     TEXT,
				deposit_tx_hash        TEXT,
				error_message          TEXT,
				cardano_offramp_tx_hash TEXT,
				refund_address         TEXT NOT NULL DEFAULT '',
				expires_at             INTEGER NOT NULL DEFAULT 0
			);"
		).expect("failed to create swap tables");
		SwapDb { conn: Mutex::new(conn) }
	}

	pub fn insert(&self, mapping: &SwapMapping) {
		let conn = self.conn.lock().unwrap();
		conn.execute(
			"INSERT INTO swap_mappings (payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, create_tx_hash)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
			params![
				mapping.payment_hash,
				mapping.invoice_id,
				mapping.amount_cbtc,
				mapping.cardano_address,
				mapping.status.as_str(),
				mapping.created_at,
				mapping.expires_at,
				mapping.create_tx_hash,
			],
		).expect("failed to insert swap mapping");
	}

	pub fn get_by_payment_hash(&self, payment_hash: &str) -> Option<SwapMapping> {
		let conn = self.conn.lock().unwrap();
		conn.query_row(
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash, create_tx_hash
			 FROM swap_mappings WHERE payment_hash = ?1",
			params![payment_hash],
			|row| Ok(row_to_swap(row)),
		).ok()
	}

	pub fn update_status(&self, payment_hash: &str, status: SwapStatus, tx_hash: Option<&str>) {
		let conn = self.conn.lock().unwrap();
		conn.execute(
			"UPDATE swap_mappings SET status = ?1, cardano_tx_hash = ?2 WHERE payment_hash = ?3",
			params![status.as_str(), tx_hash, payment_hash],
		).expect("failed to update swap status");
	}

	pub fn get_expired_pending(&self, now_ms: i64) -> Vec<SwapMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash, create_tx_hash
			 FROM swap_mappings WHERE status = 'pending' AND expires_at < ?1"
		).expect("failed to prepare expired query");

		stmt.query_map(params![now_ms], |row| {
			Ok(row_to_swap(row))
		}).expect("failed to query expired mappings")
		.filter_map(|r| r.ok())
		.collect()
	}

	// ─── Offramp methods ─────────────────────────────────────────────────────

	pub fn insert_offramp(&self, mapping: &OfframpMapping) {
		let conn = self.conn.lock().unwrap();
		conn.execute(
			"INSERT INTO offramp_mappings (offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at, cardano_offramp_tx_hash, refund_address, expires_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
			params![
				mapping.offramp_id,
				mapping.bolt11,
				mapping.payment_hash,
				mapping.amount_cbtc,
				mapping.cbtc_tx_hash,
				mapping.status.as_str(),
				mapping.created_at,
				mapping.cardano_offramp_tx_hash,
				mapping.refund_address,
				mapping.expires_at,
			],
		).expect("failed to insert offramp mapping");
	}

	pub fn get_offramp_by_id(&self, offramp_id: i64) -> Option<OfframpMapping> {
		let conn = self.conn.lock().unwrap();
		conn.query_row(
			"SELECT offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at,
			        lightning_preimage, deposit_tx_hash, error_message, cardano_offramp_tx_hash,
			        refund_address, expires_at
			 FROM offramp_mappings WHERE offramp_id = ?1",
			params![offramp_id],
			|row| Ok(row_to_offramp(row)),
		).ok()
	}

	pub fn get_offramp_by_payment_hash(&self, payment_hash: &str) -> Option<OfframpMapping> {
		let conn = self.conn.lock().unwrap();
		conn.query_row(
			"SELECT offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at,
			        lightning_preimage, deposit_tx_hash, error_message, cardano_offramp_tx_hash,
			        refund_address, expires_at
			 FROM offramp_mappings WHERE payment_hash = ?1",
			params![payment_hash],
			|row| Ok(row_to_offramp(row)),
		).ok()
	}

	pub fn update_offramp_status(
		&self, offramp_id: i64, status: OfframpStatus,
		preimage: Option<&str>, deposit_tx_hash: Option<&str>, error_message: Option<&str>,
	) {
		let conn = self.conn.lock().unwrap();
		conn.execute(
			"UPDATE offramp_mappings
			 SET status = ?1, lightning_preimage = COALESCE(?2, lightning_preimage),
			     deposit_tx_hash = COALESCE(?3, deposit_tx_hash),
			     error_message = COALESCE(?4, error_message)
			 WHERE offramp_id = ?5",
			params![status.as_str(), preimage, deposit_tx_hash, error_message, offramp_id],
		).expect("failed to update offramp status");
	}

	pub fn update_offramp_cbtc_tx(&self, offramp_id: i64, cbtc_tx_hash: &str) {
		let conn = self.conn.lock().unwrap();
		conn.execute(
			"UPDATE offramp_mappings SET cbtc_tx_hash = ?1 WHERE offramp_id = ?2",
			params![cbtc_tx_hash, offramp_id],
		).expect("failed to update offramp cbtc_tx_hash");
	}

	/// Get expired offramp mappings (awaiting_deposit or failed, past expires_at).
	pub fn get_expired_offramps(&self, now_ms: i64) -> Vec<OfframpMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at,
			        lightning_preimage, deposit_tx_hash, error_message, cardano_offramp_tx_hash,
			        refund_address, expires_at
			 FROM offramp_mappings
			 WHERE status IN ('awaiting_deposit', 'failed') AND expires_at > 0 AND expires_at < ?1"
		).expect("failed to prepare expired offramp query");

		stmt.query_map(params![now_ms], |row| {
			Ok(row_to_offramp(row))
		}).expect("failed to query expired offramps")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// List recent onramp swaps, ordered by creation time (newest first).
	pub fn list_recent_swaps(&self, limit: i64) -> Vec<SwapMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash, create_tx_hash
			 FROM swap_mappings ORDER BY created_at DESC LIMIT ?1"
		).expect("failed to prepare recent swaps query");

		stmt.query_map(params![limit], |row| {
			Ok(row_to_swap(row))
		}).expect("failed to query recent swaps")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// List recent offramp swaps, ordered by creation time (newest first).
	pub fn list_recent_offramps(&self, limit: i64) -> Vec<OfframpMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at,
			        lightning_preimage, deposit_tx_hash, error_message, cardano_offramp_tx_hash,
			        refund_address, expires_at
			 FROM offramp_mappings ORDER BY created_at DESC LIMIT ?1"
		).expect("failed to prepare recent offramps query");

		stmt.query_map(params![limit], |row| {
			Ok(row_to_offramp(row))
		}).expect("failed to query recent offramps")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// Get counts of swaps by status.
	pub fn get_swap_counts(&self) -> Vec<(String, i64)> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT status, COUNT(*) FROM swap_mappings GROUP BY status"
		).expect("failed to prepare swap counts query");

		stmt.query_map([], |row| {
			Ok((row.get::<_, String>(0).unwrap(), row.get::<_, i64>(1).unwrap()))
		}).expect("failed to query swap counts")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// Get counts of offramps by status.
	pub fn get_offramp_counts(&self) -> Vec<(String, i64)> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT status, COUNT(*) FROM offramp_mappings GROUP BY status"
		).expect("failed to prepare offramp counts query");

		stmt.query_map([], |row| {
			Ok((row.get::<_, String>(0).unwrap(), row.get::<_, i64>(1).unwrap()))
		}).expect("failed to query offramp counts")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// Get swaps stuck in a given status (for crash recovery).
	pub fn get_by_status(&self, status: SwapStatus) -> Vec<SwapMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash, create_tx_hash
			 FROM swap_mappings WHERE status = ?1"
		).expect("failed to prepare status query");

		stmt.query_map(params![status.as_str()], |row| {
			Ok(row_to_swap(row))
		}).expect("failed to query by status")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// Get offramps stuck in a given status (for crash recovery).
	pub fn get_offramps_by_status(&self, status: OfframpStatus) -> Vec<OfframpMapping> {
		let conn = self.conn.lock().unwrap();
		let mut stmt = conn.prepare(
			"SELECT offramp_id, bolt11, payment_hash, amount_cbtc, cbtc_tx_hash, status, created_at,
			        lightning_preimage, deposit_tx_hash, error_message, cardano_offramp_tx_hash,
			        refund_address, expires_at
			 FROM offramp_mappings WHERE status = ?1"
		).expect("failed to prepare offramp status query");

		stmt.query_map(params![status.as_str()], |row| {
			Ok(row_to_offramp(row))
		}).expect("failed to query offramps by status")
		.filter_map(|r| r.ok())
		.collect()
	}

	/// Get the next offramp ID (max + 1).
	pub fn next_offramp_id(&self) -> i64 {
		let conn = self.conn.lock().unwrap();
		conn.query_row(
			"SELECT COALESCE(MAX(offramp_id), 0) + 1 FROM offramp_mappings",
			[],
			|row| row.get(0),
		).unwrap_or(1)
	}
}

// ─── Offramp types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OfframpStatus {
	/// CreateOfframp TX submitted, awaiting user cBTC deposit.
	AwaitingDeposit,
	/// cBTC TX submitted, verifying receipt at operator address.
	PendingVerification,
	/// cBTC verified, paying user's Lightning invoice.
	PayingLightning,
	/// Lightning payment sent, depositing cBTC to pool via FulfillOfframp.
	DepositingToPool,
	/// Deposit complete. Offramp finished.
	Completed,
	/// Failed at some step.
	Failed,
}

impl OfframpStatus {
	pub fn as_str(&self) -> &'static str {
		match self {
			OfframpStatus::AwaitingDeposit => "awaiting_deposit",
			OfframpStatus::PendingVerification => "pending_verification",
			OfframpStatus::PayingLightning => "paying_lightning",
			OfframpStatus::DepositingToPool => "depositing_to_pool",
			OfframpStatus::Completed => "completed",
			OfframpStatus::Failed => "failed",
		}
	}

	pub fn from_str(s: &str) -> Self {
		match s {
			"awaiting_deposit" => OfframpStatus::AwaitingDeposit,
			"pending_verification" => OfframpStatus::PendingVerification,
			"paying_lightning" => OfframpStatus::PayingLightning,
			"depositing_to_pool" => OfframpStatus::DepositingToPool,
			"completed" => OfframpStatus::Completed,
			"failed" => OfframpStatus::Failed,
			_ => OfframpStatus::Failed,
		}
	}
}

#[derive(Debug, Clone)]
pub(crate) struct OfframpMapping {
	pub offramp_id: i64,
	pub bolt11: String,
	pub payment_hash: String,
	pub amount_cbtc: i64,
	pub cbtc_tx_hash: String,
	pub status: OfframpStatus,
	pub created_at: i64,
	pub lightning_preimage: Option<String>,
	pub deposit_tx_hash: Option<String>,
	pub error_message: Option<String>,
	pub cardano_offramp_tx_hash: Option<String>,
	pub refund_address: String,
	pub expires_at: i64,
}

fn row_to_swap(row: &rusqlite::Row) -> SwapMapping {
	SwapMapping {
		payment_hash: row.get(0).unwrap(),
		invoice_id: row.get(1).unwrap(),
		amount_cbtc: row.get(2).unwrap(),
		cardano_address: row.get(3).unwrap(),
		status: SwapStatus::from_str(&row.get::<_, String>(4).unwrap()),
		created_at: row.get(5).unwrap(),
		expires_at: row.get(6).unwrap(),
		cardano_tx_hash: row.get(7).unwrap(),
		create_tx_hash: row.get(8).unwrap_or(None),
	}
}

fn row_to_offramp(row: &rusqlite::Row) -> OfframpMapping {
	OfframpMapping {
		offramp_id: row.get(0).unwrap(),
		bolt11: row.get(1).unwrap(),
		payment_hash: row.get(2).unwrap(),
		amount_cbtc: row.get(3).unwrap(),
		cbtc_tx_hash: row.get(4).unwrap(),
		status: OfframpStatus::from_str(&row.get::<_, String>(5).unwrap()),
		created_at: row.get(6).unwrap(),
		lightning_preimage: row.get(7).unwrap(),
		deposit_tx_hash: row.get(8).unwrap(),
		error_message: row.get(9).unwrap(),
		cardano_offramp_tx_hash: row.get(10).unwrap(),
		refund_address: row.get::<_, String>(11).unwrap_or_default(),
		expires_at: row.get(12).unwrap_or(0),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// ─── Status enum round-trips ────────────────────────────────────────────

	#[test]
	fn swap_status_roundtrip() {
		let variants = [
			SwapStatus::Pending,
			SwapStatus::Fulfilling,
			SwapStatus::Completed,
			SwapStatus::Failed,
			SwapStatus::Expired,
		];
		for v in &variants {
			assert_eq!(SwapStatus::from_str(v.as_str()), *v);
		}
	}

	#[test]
	fn swap_status_unknown_falls_back_to_failed() {
		assert_eq!(SwapStatus::from_str("garbage"), SwapStatus::Failed);
		assert_eq!(SwapStatus::from_str(""), SwapStatus::Failed);
	}

	#[test]
	fn offramp_status_roundtrip() {
		let variants = [
			OfframpStatus::AwaitingDeposit,
			OfframpStatus::PendingVerification,
			OfframpStatus::PayingLightning,
			OfframpStatus::DepositingToPool,
			OfframpStatus::Completed,
			OfframpStatus::Failed,
		];
		for v in &variants {
			assert_eq!(OfframpStatus::from_str(v.as_str()), *v);
		}
	}

	#[test]
	fn offramp_status_unknown_falls_back_to_failed() {
		assert_eq!(OfframpStatus::from_str("xyz"), OfframpStatus::Failed);
		assert_eq!(OfframpStatus::from_str(""), OfframpStatus::Failed);
	}

	// ─── SwapDb helpers ─────────────────────────────────────────────────────

	fn test_db() -> SwapDb {
		SwapDb::open(":memory:")
	}

	fn make_swap(hash: &str, expires_at: i64) -> SwapMapping {
		SwapMapping {
			payment_hash: hash.to_string(),
			invoice_id: 1,
			amount_cbtc: 100_000,
			cardano_address: "addr_test1qz...".to_string(),
			status: SwapStatus::Pending,
			created_at: 1_000_000,
			expires_at,
			cardano_tx_hash: None,
			create_tx_hash: None,
		}
	}

	fn make_offramp(id: i64, hash: &str) -> OfframpMapping {
		OfframpMapping {
			offramp_id: id,
			bolt11: "lnbc1...".to_string(),
			payment_hash: hash.to_string(),
			amount_cbtc: 50_000,
			cbtc_tx_hash: String::new(),
			status: OfframpStatus::AwaitingDeposit,
			created_at: 1_000_000,
			lightning_preimage: None,
			deposit_tx_hash: None,
			error_message: None,
			cardano_offramp_tx_hash: None,
			refund_address: "addr_test1qz...".to_string(),
			expires_at: 2_000_000,
		}
	}

	// ─── Onramp CRUD ────────────────────────────────────────────────────────

	#[test]
	fn insert_and_get_by_payment_hash() {
		let db = test_db();
		let swap = make_swap("aabb01", 9_999_999);
		db.insert(&swap);

		let got = db.get_by_payment_hash("aabb01").unwrap();
		assert_eq!(got.payment_hash, "aabb01");
		assert_eq!(got.invoice_id, 1);
		assert_eq!(got.amount_cbtc, 100_000);
		assert_eq!(got.status, SwapStatus::Pending);
		assert!(got.cardano_tx_hash.is_none());
	}

	#[test]
	fn get_nonexistent_returns_none() {
		let db = test_db();
		assert!(db.get_by_payment_hash("does_not_exist").is_none());
	}

	#[test]
	fn update_status_changes_status_and_tx_hash() {
		let db = test_db();
		db.insert(&make_swap("hash01", 9_999_999));

		db.update_status("hash01", SwapStatus::Completed, Some("cardano_tx_abc"));
		let got = db.get_by_payment_hash("hash01").unwrap();
		assert_eq!(got.status, SwapStatus::Completed);
		assert_eq!(got.cardano_tx_hash.as_deref(), Some("cardano_tx_abc"));
	}

	#[test]
	fn get_expired_pending_filters_correctly() {
		let db = test_db();
		let now = 5_000_000;

		// expired + pending → should be returned
		db.insert(&make_swap("expired_pending", now - 1));
		// not expired + pending → should NOT be returned
		db.insert(&make_swap("future_pending", now + 1_000));
		// expired + completed → should NOT be returned
		let mut completed = make_swap("expired_completed", now - 1);
		completed.payment_hash = "expired_completed".to_string();
		db.insert(&completed);
		db.update_status("expired_completed", SwapStatus::Completed, None);

		let expired = db.get_expired_pending(now);
		assert_eq!(expired.len(), 1);
		assert_eq!(expired[0].payment_hash, "expired_pending");
	}

	#[test]
	fn get_expired_pending_empty_when_none() {
		let db = test_db();
		// Insert a non-expired pending swap
		db.insert(&make_swap("future", 99_999_999));
		assert!(db.get_expired_pending(1_000_000).is_empty());
	}

	// ─── Offramp CRUD ───────────────────────────────────────────────────────

	#[test]
	fn insert_offramp_and_get_by_id() {
		let db = test_db();
		let m = make_offramp(1, "off_hash_01");
		db.insert_offramp(&m);

		let got = db.get_offramp_by_id(1).unwrap();
		assert_eq!(got.offramp_id, 1);
		assert_eq!(got.payment_hash, "off_hash_01");
		assert_eq!(got.amount_cbtc, 50_000);
		assert_eq!(got.status, OfframpStatus::AwaitingDeposit);
		assert!(got.lightning_preimage.is_none());
	}

	#[test]
	fn get_offramp_by_payment_hash() {
		let db = test_db();
		db.insert_offramp(&make_offramp(1, "hash_lookup"));

		let got = db.get_offramp_by_payment_hash("hash_lookup").unwrap();
		assert_eq!(got.offramp_id, 1);
	}

	#[test]
	fn get_offramp_nonexistent_returns_none() {
		let db = test_db();
		assert!(db.get_offramp_by_id(999).is_none());
	}

	#[test]
	fn update_offramp_status_with_preimage() {
		let db = test_db();
		db.insert_offramp(&make_offramp(1, "off01"));

		db.update_offramp_status(1, OfframpStatus::Completed, Some("preimage_hex"), None, None);
		let got = db.get_offramp_by_id(1).unwrap();
		assert_eq!(got.status, OfframpStatus::Completed);
		assert_eq!(got.lightning_preimage.as_deref(), Some("preimage_hex"));
	}

	#[test]
	fn update_offramp_status_coalesce_preserves_existing() {
		let db = test_db();
		db.insert_offramp(&make_offramp(1, "off02"));

		// First update sets preimage
		db.update_offramp_status(1, OfframpStatus::PayingLightning, Some("pre_img"), None, None);
		// Second update passes None for preimage → COALESCE keeps old value
		db.update_offramp_status(1, OfframpStatus::Completed, None, Some("dep_tx"), None);

		let got = db.get_offramp_by_id(1).unwrap();
		assert_eq!(got.status, OfframpStatus::Completed);
		assert_eq!(got.lightning_preimage.as_deref(), Some("pre_img"));
		assert_eq!(got.deposit_tx_hash.as_deref(), Some("dep_tx"));
	}

	#[test]
	fn update_offramp_cbtc_tx() {
		let db = test_db();
		db.insert_offramp(&make_offramp(1, "off03"));

		db.update_offramp_cbtc_tx(1, "cbtc_tx_hash_xyz");
		let got = db.get_offramp_by_id(1).unwrap();
		assert_eq!(got.cbtc_tx_hash, "cbtc_tx_hash_xyz");
	}

	#[test]
	fn next_offramp_id_starts_at_1() {
		let db = test_db();
		assert_eq!(db.next_offramp_id(), 1);
	}

	#[test]
	fn next_offramp_id_increments() {
		let db = test_db();
		db.insert_offramp(&make_offramp(1, "a"));
		db.insert_offramp(&make_offramp(2, "b"));
		assert_eq!(db.next_offramp_id(), 3);
	}
}
