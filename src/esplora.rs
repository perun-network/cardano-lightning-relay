//! Esplora-based chain source — replaces BitcoindClient for environments without bitcoind.
//!
//! Queries a public or self-hosted Esplora-compatible REST API (e.g. mempool.space)
//! for block data, fee estimates, and TX broadcast. No local bitcoind needed.
//!
//! Enabled via `BITCOIN_BACKEND=esplora` + `ESPLORA_URL=https://mempool.space/signet/api`.
//!
//! Wallet operations (UTXO tracking, signing) still require local keys — currently
//! delegated back to the BitcoindClient's WalletSource. Future: BDK wallet.

use bitcoin::consensus::encode;
use bitcoin::hash_types::BlockHash;
use bitcoin::Transaction;
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::disk::FilesystemLogger;

/// Esplora REST API client for chain data, fee estimation, and TX broadcast.
pub struct EsploraClient {
	base_url: String,
	http: reqwest::Client,
	fees: Arc<HashMap<ConfirmationTarget, AtomicU32>>,
	logger: Arc<FilesystemLogger>,
}

const MIN_FEERATE: u32 = 253;

impl EsploraClient {
	pub async fn new(
		base_url: String, logger: Arc<FilesystemLogger>,
	) -> Result<Self, Box<dyn std::error::Error>> {
		let base_url = base_url.trim_end_matches('/').to_string();
		let http = reqwest::Client::builder()
			.timeout(std::time::Duration::from_secs(30))
			.build()?;

		// Verify connectivity
		let resp = http.get(format!("{}/blocks/tip/height", base_url)).send().await?;
		if !resp.status().is_success() {
			return Err(format!("Esplora API unreachable at {}", base_url).into());
		}
		let tip_height: u64 = resp.text().await?.trim().parse()?;
		println!("Esplora connected: {} (tip height: {})", base_url, tip_height);

		let mut fees = HashMap::new();
		fees.insert(ConfirmationTarget::MaximumFeeEstimate, AtomicU32::new(50000));
		fees.insert(ConfirmationTarget::UrgentOnChainSweep, AtomicU32::new(5000));
		fees.insert(
			ConfirmationTarget::MinAllowedAnchorChannelRemoteFee,
			AtomicU32::new(MIN_FEERATE),
		);
		fees.insert(
			ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee,
			AtomicU32::new(MIN_FEERATE),
		);
		fees.insert(
			ConfirmationTarget::AnchorChannelFee,
			AtomicU32::new(MIN_FEERATE),
		);
		fees.insert(
			ConfirmationTarget::NonAnchorChannelFee,
			AtomicU32::new(2000),
		);
		fees.insert(
			ConfirmationTarget::ChannelCloseMinimum,
			AtomicU32::new(MIN_FEERATE),
		);
		fees.insert(
			ConfirmationTarget::OutputSpendingFee,
			AtomicU32::new(MIN_FEERATE),
		);
		let fees = Arc::new(fees);

		let client = Self { base_url, http, fees: Arc::clone(&fees), logger };

		// Initial fee estimate
		client.update_fee_estimates().await;

		Ok(client)
	}

	/// Fetch fee estimates from Esplora and update cached values.
	pub async fn update_fee_estimates(&self) {
		let url = format!("{}/fee-estimates", self.base_url);
		let resp = match self.http.get(&url).send().await {
			Ok(r) => r,
			Err(e) => {
				lightning::log_warn!(&*self.logger, "Failed to fetch fee estimates: {}", e);
				return;
			},
		};

		let estimates: HashMap<String, f64> = match resp.json().await {
			Ok(e) => e,
			Err(e) => {
				lightning::log_warn!(&*self.logger, "Failed to parse fee estimates: {}", e);
				return;
			},
		};

		// Esplora returns { "1": rate, "2": rate, ... } where keys are confirmation targets
		// in blocks and values are sat/vB.
		let sat_vb_to_sat_kwu = |sat_vb: f64| -> u32 {
			// 1 sat/vB = 250 sat/kWU
			std::cmp::max((sat_vb * 250.0) as u32, MIN_FEERATE)
		};

		if let Some(rate) = estimates.get("1") {
			self.fees
				.get(&ConfirmationTarget::UrgentOnChainSweep)
				.unwrap()
				.store(sat_vb_to_sat_kwu(*rate), Ordering::Release);
		}
		if let Some(rate) = estimates.get("6") {
			self.fees
				.get(&ConfirmationTarget::NonAnchorChannelFee)
				.unwrap()
				.store(sat_vb_to_sat_kwu(*rate), Ordering::Release);
			self.fees
				.get(&ConfirmationTarget::ChannelCloseMinimum)
				.unwrap()
				.store(sat_vb_to_sat_kwu(*rate), Ordering::Release);
		}
		if let Some(rate) = estimates.get("25") {
			self.fees
				.get(&ConfirmationTarget::OutputSpendingFee)
				.unwrap()
				.store(sat_vb_to_sat_kwu(*rate), Ordering::Release);
		}
	}

	/// Broadcast a raw transaction via Esplora POST /tx.
	pub async fn broadcast_tx(&self, tx: &Transaction) -> Result<(), String> {
		let tx_hex = encode::serialize_hex(tx);
		let url = format!("{}/tx", self.base_url);
		let resp = self
			.http
			.post(&url)
			.body(tx_hex)
			.send()
			.await
			.map_err(|e| format!("broadcast failed: {}", e))?;

		if resp.status().is_success() {
			Ok(())
		} else {
			let body = resp.text().await.unwrap_or_default();
			Err(format!("broadcast rejected: {}", body))
		}
	}

	/// Get the current tip height.
	pub async fn get_tip_height(&self) -> Result<u64, String> {
		let url = format!("{}/blocks/tip/height", self.base_url);
		let resp = self
			.http
			.get(&url)
			.send()
			.await
			.map_err(|e| format!("tip height query failed: {}", e))?;
		let text = resp.text().await.map_err(|e| format!("tip height parse failed: {}", e))?;
		text.trim().parse().map_err(|e| format!("tip height not a number: {}", e))
	}

	/// Get the current tip block hash.
	pub async fn get_tip_hash(&self) -> Result<BlockHash, String> {
		let url = format!("{}/blocks/tip/hash", self.base_url);
		let resp = self
			.http
			.get(&url)
			.send()
			.await
			.map_err(|e| format!("tip hash query failed: {}", e))?;
		let text = resp.text().await.map_err(|e| format!("tip hash parse failed: {}", e))?;
		text.trim()
			.parse()
			.map_err(|e| format!("tip hash parse failed: {}", e))
	}

	/// Get the wallet balance for an address (sum of confirmed UTXOs).
	pub async fn get_address_balance_sats(&self, address: &str) -> u64 {
		let url = format!("{}/address/{}/utxo", self.base_url, address);
		let resp = match self.http.get(&url).send().await {
			Ok(r) => r,
			Err(_) => return 0,
		};
		let utxos: Vec<serde_json::Value> = match resp.json().await {
			Ok(u) => u,
			Err(_) => return 0,
		};
		utxos
			.iter()
			.filter(|u| {
				u.get("status")
					.and_then(|s| s.get("confirmed"))
					.and_then(|c| c.as_bool())
					.unwrap_or(false)
			})
			.filter_map(|u| u.get("value").and_then(|v| v.as_u64()))
			.sum()
	}
}

impl FeeEstimator for EsploraClient {
	fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
		self.fees
			.get(&confirmation_target)
			.unwrap()
			.load(Ordering::Acquire)
	}
}

impl BroadcasterInterface for EsploraClient {
	fn broadcast_transactions(&self, txs: &[&Transaction]) {
		let txs: Vec<Transaction> = txs.iter().map(|t| (*t).clone()).collect();
		let base_url = self.base_url.clone();
		let http = self.http.clone();
		let logger = Arc::clone(&self.logger);

		tokio::spawn(async move {
			for tx in &txs {
				let tx_hex = encode::serialize_hex(tx);
				let url = format!("{}/tx", base_url);
				match http.post(&url).body(tx_hex).send().await {
					Ok(resp) if resp.status().is_success() => {
						lightning::log_info!(
							&*logger,
							"Broadcast TX {} via Esplora",
							tx.compute_txid()
						);
					},
					Ok(resp) => {
						let body = resp.text().await.unwrap_or_default();
						lightning::log_warn!(
							&*logger,
							"Warning, failed to broadcast TX {}: {}",
							tx.compute_txid(),
							body
						);
					},
					Err(e) => {
						lightning::log_warn!(
							&*logger,
							"Warning, failed to broadcast TX {}: {}",
							tx.compute_txid(),
							e
						);
					},
				}
			}
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_esplora_signet_connectivity() {
		let url = std::env::var("ESPLORA_URL")
			.unwrap_or_else(|_| "https://mempool.space/signet/api".into());

		let ldk_data_dir = "/tmp/test_esplora_ldk".to_string();
		std::fs::create_dir_all(format!("{}/.ldk/logs", ldk_data_dir)).ok();
		let logger = Arc::new(crate::disk::FilesystemLogger::new(ldk_data_dir));

		let client = EsploraClient::new(url, logger).await.expect("should connect");

		let height = client.get_tip_height().await.expect("should get tip");
		assert!(height > 200_000, "Signet tip should be > 200K (got {})", height);

		let hash = client.get_tip_hash().await.expect("should get hash");
		println!("Signet tip: height={} hash={}", height, hash);

		// Check our relay wallet address
		let sats = client.get_address_balance_sats("tb1qyjw7zj6xctmf4qsk0a2e08y7d3uue05yq6fjun").await;
		println!("Relay wallet: {} sats", sats);

		// Fee estimation should return non-zero
		let fee = client.get_est_sat_per_1000_weight(ConfirmationTarget::NonAnchorChannelFee);
		assert!(fee >= MIN_FEERATE, "fee should be >= MIN_FEERATE");
		println!("NonAnchorChannelFee: {} sat/kWU", fee);
	}
}
