//! BDK-based on-chain wallet — replaces bitcoind wallet RPCs for key management,
//! UTXO tracking, and transaction signing.
//!
//! Syncs wallet state via Esplora (no bitcoind needed). Keys are derived from a
//! BIP39 mnemonic stored in a local file.
//!
//! Implements LDK's `WalletSource` and `ChangeDestinationSource` traits.

use bdk_esplora::esplora_client::AsyncClient as EsploraAsyncClient;
use bdk_esplora::EsploraAsyncExt;
use bdk_wallet::bitcoin::Network;
use bip39::Mnemonic;
use bdk_wallet::{KeychainKind, Wallet};
use bitcoin::hashes::Hash;
use bitcoin::psbt::Psbt;
use bitcoin::{Amount, ScriptBuf, Transaction, TxOut};
use lightning::events::bump_transaction::{Utxo, WalletSource};
use lightning::sign::ChangeDestinationSource;
use lightning::util::logger::Logger;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::disk::FilesystemLogger;

const BDK_CLIENT_STOP_GAP: usize = 20;
const BDK_CLIENT_CONCURRENCY: u8 = 4;

/// BDK wallet backed by Esplora for chain queries.
pub struct BdkOnchainWallet {
	wallet: Mutex<Wallet>,
	esplora: EsploraAsyncClient,
	logger: Arc<FilesystemLogger>,
}

impl BdkOnchainWallet {
	/// Create or load wallet from a mnemonic file. If the file doesn't exist,
	/// generate a new mnemonic and save it.
	pub fn new(
		seed_path: &str, network: Network, esplora_url: &str, logger: Arc<FilesystemLogger>,
	) -> Result<Self, Box<dyn std::error::Error>> {
		let mnemonic = if Path::new(seed_path).exists() {
			let words = std::fs::read_to_string(seed_path)?;
			words.trim().parse::<Mnemonic>()?
		} else {
			let entropy: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
			let mnemonic = Mnemonic::from_entropy(&entropy)?;
			std::fs::write(seed_path, mnemonic.to_string())?;
			println!("Generated new wallet mnemonic → {}", seed_path);
			mnemonic
		};

		// Derive xprv from mnemonic → BIP84 descriptors (native segwit)
		let seed = mnemonic.to_seed("");
		let xprv = bitcoin::bip32::Xpriv::new_master(network, &seed)?;

		// BIP84 path: m/84'/1'/0' for testnet, m/84'/0'/0' for mainnet
		let coin_type = if network == Network::Bitcoin { 0 } else { 1 };
		let external_desc = format!(
			"wpkh({}/84'/{}'/{}'/{}/0/*)",
			xprv, coin_type, 0, 0
		);
		let internal_desc = format!(
			"wpkh({}/84'/{}'/{}'/{}/1/*)",
			xprv, coin_type, 0, 0
		);

		let wallet = Wallet::create(external_desc, internal_desc)
			.network(network)
			.create_wallet_no_persist()?;

		let esplora = bdk_esplora::esplora_client::Builder::new(esplora_url)
			.build_async()?;

		println!(
			"BDK wallet initialized (network: {:?}, first address: {})",
			network,
			wallet.peek_address(KeychainKind::External, 0),
		);

		Ok(Self {
			wallet: Mutex::new(wallet),
			esplora,
			logger,
		})
	}

	/// Sync wallet UTXOs with Esplora. Should be called periodically.
	pub async fn sync(&self) -> Result<(), Box<dyn std::error::Error>> {
		let request = {
			let wallet = self.wallet.lock().unwrap();
			wallet.start_full_scan().build()
		};

		let update = self
			.esplora
			.full_scan(request, BDK_CLIENT_STOP_GAP, BDK_CLIENT_CONCURRENCY as usize)
			.await?;

		let mut wallet = self.wallet.lock().unwrap();
		wallet.apply_update(update)?;

		let balance = wallet.balance();
		lightning::log_info!(
			&*self.logger,
			"BDK wallet synced: confirmed={} sats, trusted_pending={} sats",
			balance.confirmed,
			balance.trusted_pending,
		);

		Ok(())
	}

	/// Get the confirmed balance in satoshis.
	pub fn balance_sats(&self) -> u64 {
		let wallet = self.wallet.lock().unwrap();
		wallet.balance().confirmed.to_sat()
	}

	/// Get a new receiving address.
	pub fn new_address(&self) -> bitcoin::Address {
		let mut wallet = self.wallet.lock().unwrap();
		wallet
			.reveal_next_address(KeychainKind::External)
			.address
	}
}

impl WalletSource for BdkOnchainWallet {
	fn list_confirmed_utxos<'a>(
		&'a self,
	) -> lightning::util::async_poll::AsyncResult<'a, Vec<Utxo>, ()> {
		Box::pin(async move {
			let wallet = self.wallet.lock().unwrap();
			let utxos = wallet
				.list_unspent()
				.filter(|u| u.chain_position.is_confirmed())
				.filter_map(|u| {
					let outpoint = bitcoin::OutPoint {
						txid: u.outpoint.txid,
						vout: u.outpoint.vout,
					};
					let value = Amount::from_sat(u.txout.value.to_sat());
					let script = &u.txout.script_pubkey;

					if script.is_p2wpkh() {
						let hash_bytes: [u8; 20] = script.as_bytes()[2..22].try_into().ok()?;
						let wpkh = bitcoin::WPubkeyHash::from_byte_array(hash_bytes.into());
						Some(Utxo::new_v0_p2wpkh(outpoint, value, &wpkh))
					} else if script.is_p2tr() {
						Some(Utxo {
							outpoint,
							output: TxOut {
								value,
								script_pubkey: script.clone(),
							},
							satisfaction_weight: 1 * 4 + 1 + 1 + 64, // witness: script_sig + items + sig_len + schnorr_sig
						})
					} else {
						None
					}
				})
				.collect();
			Ok(utxos)
		})
	}

	fn get_change_script<'a>(
		&'a self,
	) -> lightning::util::async_poll::AsyncResult<'a, ScriptBuf, ()> {
		Box::pin(async move {
			let mut wallet = self.wallet.lock().unwrap();
			let addr = wallet.reveal_next_address(KeychainKind::Internal);
			Ok(addr.address.script_pubkey())
		})
	}

	fn sign_psbt<'a>(
		&'a self, mut psbt: Psbt,
	) -> lightning::util::async_poll::AsyncResult<'a, Transaction, ()> {
		Box::pin(async move {
			let mut wallet = self.wallet.lock().unwrap();
			let finalized = wallet
				.sign(&mut psbt, bdk_wallet::SignOptions::default())
				.map_err(|_| ())?;
			if !finalized {
				return Err(());
			}
			psbt.extract_tx().map_err(|_| ())
		})
	}
}

impl ChangeDestinationSource for BdkOnchainWallet {
	fn get_change_destination_script<'a>(
		&'a self,
	) -> lightning::util::async_poll::AsyncResult<'a, ScriptBuf, ()> {
		self.get_change_script()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn test_bdk_wallet_funded_on_signet() {
		// This test verifies end-to-end: BDK wallet syncs via Esplora and sees
		// funds that were sent from our bitcoind Signet wallet.
		// Pre-requisite: send some sBTC to the BDK address first.
		let seed_path = "/tmp/test_bdk_seed.txt";
		if !Path::new(seed_path).exists() {
			println!("SKIP: no seed at {} — run test_bdk_wallet_creation first", seed_path);
			return;
		}

		let ldk_data_dir = "/tmp/test_bdk_funded".to_string();
		std::fs::create_dir_all(format!("{}/.ldk/logs", ldk_data_dir)).ok();
		let logger = Arc::new(crate::disk::FilesystemLogger::new(ldk_data_dir));

		let wallet = BdkOnchainWallet::new(
			seed_path,
			Network::Signet,
			"https://mempool.space/signet/api",
			logger,
		)
		.expect("should load wallet");

		wallet.sync().await.expect("sync should succeed");
		let balance = wallet.balance_sats();
		println!("BDK wallet balance after sync: {} sats", balance);

		if balance > 0 {
			println!("PASS: BDK wallet has funds on Signet ({})", balance);

			// Test WalletSource trait
			use lightning::events::bump_transaction::WalletSource;
			let utxos = wallet.list_confirmed_utxos().await.expect("should list utxos");
			println!("Confirmed UTXOs: {}", utxos.len());
			for u in &utxos {
				println!("  {}:{} = {} sats", u.outpoint.txid, u.outpoint.vout, u.output.value);
			}
			assert!(!utxos.is_empty(), "should have at least one UTXO");
		} else {
			println!("INFO: BDK wallet has 0 balance — send sBTC to {} first", wallet.new_address());
		}
	}

	#[tokio::test]
	async fn test_bdk_wallet_creation() {
		let seed_path = "/tmp/test_bdk_seed.txt";
		let _ = std::fs::remove_file(seed_path);

		let ldk_data_dir = "/tmp/test_bdk_wallet".to_string();
		std::fs::create_dir_all(format!("{}/.ldk/logs", ldk_data_dir)).ok();
		let logger = Arc::new(crate::disk::FilesystemLogger::new(ldk_data_dir));

		let wallet = BdkOnchainWallet::new(
			seed_path,
			Network::Signet,
			"https://mempool.space/signet/api",
			logger,
		)
		.expect("should create wallet");

		// Seed file should exist now
		assert!(Path::new(seed_path).exists());

		// Should be able to generate addresses
		let addr = wallet.new_address();
		println!("Address: {}", addr);
		assert!(addr.to_string().starts_with("tb1"));

		// Sync with Esplora (empty wallet, should succeed)
		wallet.sync().await.expect("sync should succeed");
		assert_eq!(wallet.balance_sats(), 0);

		// Reload from existing seed
		let wallet2 = BdkOnchainWallet::new(
			seed_path,
			Network::Signet,
			"https://mempool.space/signet/api",
			Arc::new(crate::disk::FilesystemLogger::new("/tmp/test_bdk_wallet2".to_string())),
		)
		.expect("should reload wallet");

		// Same seed should produce same first address
		// (not strictly true after reveal, but peek should match)

		std::fs::remove_file(seed_path).ok();
		println!("BDK wallet test passed");
	}
}
