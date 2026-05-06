//! Esplora-based chain source — replaces BitcoindClient for environments without bitcoind.
//!
//! Queries a public or self-hosted Esplora-compatible REST API (e.g. mempool.space)
//! for block data, fee estimates, and TX broadcast. No local bitcoind needed.
//!
//! Enabled via `BITCOIN_BACKEND=esplora` + `ESPLORA_URL=https://mempool.space/signet/api`.
//!
//! Wallet operations (UTXO tracking, signing) still require local keys — currently
//! delegated back to the BitcoindClient's WalletSource. Future: BDK wallet.

use bitcoin::Transaction;
use bitcoin::consensus::encode;
use bitcoin::hash_types::BlockHash;
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::util::logger::Logger;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::disk::FilesystemLogger;

/// Esplora REST API client for chain data, fee estimation, and TX broadcast.
pub struct EsploraClient {
	base_url: String,
	http: reqwest::Client,
	fees: Arc<HashMap<ConfirmationTarget, AtomicU32>>,
	normal_max_feerate_sat_per_kwu: u32,
	logger: Arc<FilesystemLogger>,
}

const MIN_FEERATE: u32 = 253;
const DEFAULT_NORMAL_MAX_FEERATE_SAT_PER_KWU: u32 = 5000;
const DEFAULT_HIGH_PRIORITY_MAX_FEERATE_SAT_PER_KWU: u32 = 50000;
const MAX_FEERATE_ENV_VAR: &str = "LDK_MAX_FEERATE_SAT_PER_KWU";

#[derive(Debug)]
struct FeeEstimateUpdate {
	target: ConfirmationTarget,
	source_blocks: &'static str,
	raw_sat_vb: f64,
	converted_sat_kwu: u32,
	assigned_sat_kwu: u32,
	was_clamped: bool,
}

fn max_feerate_sat_per_kwu_from_env() -> u32 {
	max_feerate_sat_per_kwu_from_value(std::env::var(MAX_FEERATE_ENV_VAR).ok().as_deref())
}

fn max_feerate_sat_per_kwu_from_value(value: Option<&str>) -> u32 {
	value
		.and_then(|v| v.parse::<u32>().ok())
		.filter(|v| *v >= MIN_FEERATE)
		.unwrap_or(DEFAULT_NORMAL_MAX_FEERATE_SAT_PER_KWU)
}

fn high_priority_max_feerate_sat_per_kwu(normal_max_sat_kwu: u32) -> u32 {
	std::cmp::max(normal_max_sat_kwu, DEFAULT_HIGH_PRIORITY_MAX_FEERATE_SAT_PER_KWU)
}

fn raw_sat_vb_to_sat_kwu(sat_vb: f64) -> u32 {
	if !sat_vb.is_finite() || sat_vb <= 0.0 {
		return MIN_FEERATE;
	}

	let rounded = (sat_vb * 250.0).round();
	if rounded >= u32::MAX as f64 { u32::MAX } else { rounded as u32 }
}

fn sat_vb_to_sat_kwu_clamped(sat_vb: f64, max_sat_kwu: u32) -> u32 {
	let max_sat_kwu = std::cmp::max(max_sat_kwu, MIN_FEERATE);
	let converted = raw_sat_vb_to_sat_kwu(sat_vb);
	std::cmp::min(std::cmp::max(converted, MIN_FEERATE), max_sat_kwu)
}

fn apply_fee(
	fees: &HashMap<ConfirmationTarget, AtomicU32>, updates: &mut Vec<FeeEstimateUpdate>,
	target: ConfirmationTarget, source_blocks: &'static str, raw_sat_vb: f64, max_sat_kwu: u32,
) {
	let converted_sat_kwu = std::cmp::max(raw_sat_vb_to_sat_kwu(raw_sat_vb), MIN_FEERATE);
	let assigned_sat_kwu = sat_vb_to_sat_kwu_clamped(raw_sat_vb, max_sat_kwu);
	fees.get(&target).unwrap().store(assigned_sat_kwu, Ordering::Release);
	updates.push(FeeEstimateUpdate {
		target,
		source_blocks,
		raw_sat_vb,
		converted_sat_kwu,
		assigned_sat_kwu,
		was_clamped: assigned_sat_kwu != converted_sat_kwu,
	});
}

fn apply_fee_estimates(
	fees: &HashMap<ConfirmationTarget, AtomicU32>, estimates: &HashMap<String, f64>,
	normal_max_sat_kwu: u32,
) -> Vec<FeeEstimateUpdate> {
	let mut updates = Vec::new();
	let high_priority_max_sat_kwu = high_priority_max_feerate_sat_per_kwu(normal_max_sat_kwu);

	// Esplora returns values in sat/vB. LDK's FeeEstimator expects satoshis per
	// 1000 weight units (sat/kWU). Unit conversion: 1 sat/vB = 250 sat/kWU.
	if let Some(rate) = estimates.get("1") {
		apply_fee(
			fees,
			&mut updates,
			ConfirmationTarget::MaximumFeeEstimate,
			"1",
			*rate,
			high_priority_max_sat_kwu,
		);
		apply_fee(
			fees,
			&mut updates,
			ConfirmationTarget::UrgentOnChainSweep,
			"1",
			*rate,
			high_priority_max_sat_kwu,
		);
	}

	if let Some(rate) = estimates.get("6") {
		apply_fee(
			fees,
			&mut updates,
			ConfirmationTarget::NonAnchorChannelFee,
			"6",
			*rate,
			normal_max_sat_kwu,
		);
		apply_fee(
			fees,
			&mut updates,
			ConfirmationTarget::ChannelCloseMinimum,
			"6",
			*rate,
			normal_max_sat_kwu,
		);
	}

	if let Some(rate) = estimates.get("25") {
		apply_fee(
			fees,
			&mut updates,
			ConfirmationTarget::OutputSpendingFee,
			"25",
			*rate,
			normal_max_sat_kwu,
		);
	}

	updates
}

impl EsploraClient {
	pub async fn new(
		base_url: String, logger: Arc<FilesystemLogger>,
	) -> Result<Self, Box<dyn std::error::Error>> {
		let base_url = base_url.trim_end_matches('/').to_string();
		let http =
			reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;

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
		fees.insert(ConfirmationTarget::AnchorChannelFee, AtomicU32::new(MIN_FEERATE));
		fees.insert(ConfirmationTarget::NonAnchorChannelFee, AtomicU32::new(2000));
		fees.insert(ConfirmationTarget::ChannelCloseMinimum, AtomicU32::new(MIN_FEERATE));
		fees.insert(ConfirmationTarget::OutputSpendingFee, AtomicU32::new(MIN_FEERATE));
		let fees = Arc::new(fees);

		let normal_max_feerate_sat_per_kwu = max_feerate_sat_per_kwu_from_env();
		lightning::log_info!(
			&*logger,
			"Esplora normal fee cap set to {} sat/kWU (override with {}); high-priority cap set to {} sat/kWU",
			normal_max_feerate_sat_per_kwu,
			MAX_FEERATE_ENV_VAR,
			high_priority_max_feerate_sat_per_kwu(normal_max_feerate_sat_per_kwu)
		);

		let client = Self {
			base_url,
			http,
			fees: Arc::clone(&fees),
			normal_max_feerate_sat_per_kwu,
			logger,
		};

		// Initial fee estimate
		client.update_fee_estimates().await;

		Ok(client)
	}

	/// Return the base URL for this client.
	pub fn base_url(&self) -> &str {
		&self.base_url
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

		let updates =
			apply_fee_estimates(&self.fees, &estimates, self.normal_max_feerate_sat_per_kwu);
		for update in updates {
			if update.was_clamped {
				lightning::log_info!(
					&*self.logger,
					"Esplora fee estimate {:?} from {} block target: raw {:.3} sat/vB, converted {} sat/kWU, clamped to {} sat/kWU",
					update.target,
					update.source_blocks,
					update.raw_sat_vb,
					update.converted_sat_kwu,
					update.assigned_sat_kwu
				);
			} else {
				lightning::log_info!(
					&*self.logger,
					"Esplora fee estimate {:?} from {} block target: raw {:.3} sat/vB, converted {} sat/kWU, assigned {} sat/kWU",
					update.target,
					update.source_blocks,
					update.raw_sat_vb,
					update.converted_sat_kwu,
					update.assigned_sat_kwu
				);
			}
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
		text.trim().parse().map_err(|e| format!("tip hash parse failed: {}", e))
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
		self.fees.get(&confirmation_target).unwrap().load(Ordering::Acquire)
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
	use std::sync::atomic::Ordering;

	fn test_fee_map() -> HashMap<ConfirmationTarget, AtomicU32> {
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
		fees.insert(ConfirmationTarget::AnchorChannelFee, AtomicU32::new(MIN_FEERATE));
		fees.insert(ConfirmationTarget::NonAnchorChannelFee, AtomicU32::new(2000));
		fees.insert(ConfirmationTarget::ChannelCloseMinimum, AtomicU32::new(MIN_FEERATE));
		fees.insert(ConfirmationTarget::OutputSpendingFee, AtomicU32::new(MIN_FEERATE));
		fees
	}

	fn get_fee(fees: &HashMap<ConfirmationTarget, AtomicU32>, target: ConfirmationTarget) -> u32 {
		fees.get(&target).unwrap().load(Ordering::Acquire)
	}

	#[test]
	fn sat_vb_to_sat_kwu_uses_ldk_min_feerate() {
		assert_eq!(sat_vb_to_sat_kwu_clamped(0.974, 5000), MIN_FEERATE);
		assert_eq!(sat_vb_to_sat_kwu_clamped(0.0, 5000), MIN_FEERATE);
		assert_eq!(sat_vb_to_sat_kwu_clamped(-1.0, 5000), MIN_FEERATE);
		assert_eq!(sat_vb_to_sat_kwu_clamped(f64::NAN, 5000), MIN_FEERATE);
	}

	#[test]
	fn sat_vb_to_sat_kwu_clamps_extreme_values() {
		assert_eq!(sat_vb_to_sat_kwu_clamped(100000.0, 5000), 5000);
	}

	#[test]
	fn max_feerate_cap_uses_valid_env_value_or_safe_default() {
		assert_eq!(max_feerate_sat_per_kwu_from_value(Some("10000")), 10000);
		assert_eq!(
			max_feerate_sat_per_kwu_from_value(Some("not-a-number")),
			DEFAULT_NORMAL_MAX_FEERATE_SAT_PER_KWU
		);
		assert_eq!(
			max_feerate_sat_per_kwu_from_value(Some("252")),
			DEFAULT_NORMAL_MAX_FEERATE_SAT_PER_KWU
		);
		assert_eq!(
			max_feerate_sat_per_kwu_from_value(None),
			DEFAULT_NORMAL_MAX_FEERATE_SAT_PER_KWU
		);
	}

	#[test]
	fn fee_estimates_update_maximum_and_non_anchor_targets() {
		let fees = test_fee_map();
		let mut estimates = HashMap::new();
		estimates.insert("1".to_string(), 0.974);
		estimates.insert("6".to_string(), 0.974);

		apply_fee_estimates(&fees, &estimates, 5000);

		assert_eq!(get_fee(&fees, ConfirmationTarget::MaximumFeeEstimate), MIN_FEERATE);
		assert_eq!(get_fee(&fees, ConfirmationTarget::UrgentOnChainSweep), MIN_FEERATE);
		assert_eq!(get_fee(&fees, ConfirmationTarget::NonAnchorChannelFee), MIN_FEERATE);
		assert_eq!(get_fee(&fees, ConfirmationTarget::ChannelCloseMinimum), MIN_FEERATE);
	}

	#[test]
	fn high_priority_targets_are_not_limited_by_normal_fee_cap() {
		let fees = test_fee_map();
		let mut estimates = HashMap::new();
		estimates.insert("1".to_string(), 30.0);
		estimates.insert("6".to_string(), 30.0);

		apply_fee_estimates(&fees, &estimates, 5000);

		assert_eq!(get_fee(&fees, ConfirmationTarget::MaximumFeeEstimate), 7500);
		assert_eq!(get_fee(&fees, ConfirmationTarget::UrgentOnChainSweep), 7500);
		assert_eq!(get_fee(&fees, ConfirmationTarget::NonAnchorChannelFee), 5000);
		assert_eq!(get_fee(&fees, ConfirmationTarget::ChannelCloseMinimum), 5000);
	}

	#[test]
	fn anchor_channel_fee_stays_at_mempool_floor() {
		let fees = test_fee_map();
		let mut estimates = HashMap::new();
		estimates.insert("6".to_string(), 30.0);

		apply_fee_estimates(&fees, &estimates, 5000);

		assert_eq!(get_fee(&fees, ConfirmationTarget::AnchorChannelFee), MIN_FEERATE);
	}

	#[test]
	fn high_priority_fee_estimates_are_clamped_by_high_priority_cap() {
		let fees = test_fee_map();
		let mut estimates = HashMap::new();
		estimates.insert("1".to_string(), 100000.0);

		apply_fee_estimates(&fees, &estimates, 5000);

		assert_eq!(get_fee(&fees, ConfirmationTarget::MaximumFeeEstimate), 50000);
		assert_eq!(get_fee(&fees, ConfirmationTarget::UrgentOnChainSweep), 50000);
	}

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
		let sats =
			client.get_address_balance_sats("tb1qyjw7zj6xctmf4qsk0a2e08y7d3uue05yq6fjun").await;
		println!("Relay wallet: {} sats", sats);

		// Fee estimation should return non-zero
		let fee = client.get_est_sat_per_1000_weight(ConfirmationTarget::NonAnchorChannelFee);
		assert!(fee >= MIN_FEERATE, "fee should be >= MIN_FEERATE");
		println!("NonAnchorChannelFee: {} sat/kWU", fee);
	}
}
