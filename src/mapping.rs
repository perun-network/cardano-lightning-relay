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
				cardano_tx_hash TEXT
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
			"INSERT INTO swap_mappings (payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
			params![
				mapping.payment_hash,
				mapping.invoice_id,
				mapping.amount_cbtc,
				mapping.cardano_address,
				mapping.status.as_str(),
				mapping.created_at,
				mapping.expires_at,
			],
		).expect("failed to insert swap mapping");
	}

	pub fn get_by_payment_hash(&self, payment_hash: &str) -> Option<SwapMapping> {
		let conn = self.conn.lock().unwrap();
		conn.query_row(
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash
			 FROM swap_mappings WHERE payment_hash = ?1",
			params![payment_hash],
			|row| {
				Ok(SwapMapping {
					payment_hash: row.get(0)?,
					invoice_id: row.get(1)?,
					amount_cbtc: row.get(2)?,
					cardano_address: row.get(3)?,
					status: SwapStatus::from_str(&row.get::<_, String>(4)?),
					created_at: row.get(5)?,
					expires_at: row.get(6)?,
					cardano_tx_hash: row.get(7)?,
				})
			},
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
			"SELECT payment_hash, invoice_id, amount_cbtc, cardano_address, status, created_at, expires_at, cardano_tx_hash
			 FROM swap_mappings WHERE status = 'pending' AND expires_at < ?1"
		).expect("failed to prepare expired query");

		stmt.query_map(params![now_ms], |row| {
			Ok(SwapMapping {
				payment_hash: row.get(0)?,
				invoice_id: row.get(1)?,
				amount_cbtc: row.get(2)?,
				cardano_address: row.get(3)?,
				status: SwapStatus::from_str(&row.get::<_, String>(4)?),
				created_at: row.get(5)?,
				expires_at: row.get(6)?,
				cardano_tx_hash: row.get(7)?,
			})
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
