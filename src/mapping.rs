//! SwapDb — SQLite-backed swap mapping storage.
//!
//! Tracks the lifecycle of BTC→cBTC swaps from invoice creation through
//! Lightning payment to Cardano fulfillment.

use rusqlite::{Connection, params};
use std::sync::Mutex;

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
			);"
		).expect("failed to create swap_mappings table");
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
}
