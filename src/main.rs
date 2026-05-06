mod api;
mod amounts;
mod args;
mod background;
pub mod bitcoind_client;
mod cardano_offramp;
mod cardano_ops;
mod cardano_swap;
mod cli;
mod convert;
mod bdk_wallet;
mod disk;
mod esplora;
mod events;
mod helpers;
mod hex_utils;
mod mapping;
mod recovery;
mod sweep;
mod types;

#[cfg(test)]
mod integration_tests;

use crate::bitcoind_client::BitcoindClient;
use crate::disk::FilesystemLogger;
use crate::types::{
	ChainMonitor, ChannelManager, GossipVerifier, OnionMessenger,
	OutputSweeper, OutputSweeperWrapper, PeerManager,
};
use bitcoin::io;
use bitcoin::BlockHash;
use disk::{INBOUND_PAYMENTS_FNAME, OUTBOUND_PAYMENTS_FNAME};
use lightning::chain::{chainmonitor, ChannelMonitorUpdateStatus};
use lightning::chain::BestBlock;
use lightning::events::bump_transaction::{BumpTransactionEventHandler, Wallet};
use lightning::events::Event;
use lightning::ln::channelmanager::{self, RecentPaymentDetails};
use lightning::ln::channelmanager::{
	ChainParameters, ChannelManagerReadArgs, PaymentId,
};
use lightning::ln::peer_handler::{
	IgnoringMessageHandler, MessageHandler,
};
use lightning::onion_message::messenger::OnionMessenger as LdkOnionMessenger;
use lightning::routing::gossip::P2PGossipSync;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::ProbabilisticScoringFeeParameters;
use lightning::sign::{KeysManager, NodeSigner};
use lightning::util::config::UserConfig;
use lightning::util::persist::{
	self, KVStore, MonitorUpdatingPersisterAsync, OUTPUT_SWEEPER_PERSISTENCE_KEY,
	OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lightning::util::ser::{ReadableArgs, Writeable};
use lightning::{chain};
use lightning_background_processor::{process_events_async, GossipSync, NO_LIQUIDITY_MANAGER};
use lightning_block_sync::gossip::TokioSpawner;
use lightning_block_sync::{init, poll, SpvClient, UnboundedCache};
use lightning_dns_resolver::OMDomainResolver;
use lightning_persister::fs_store::FilesystemStore;
use rand::{thread_rng, Rng};
use std::convert::TryInto;
use std::fs;
use std::fs::File;
use std::io::{BufReader, Write};
use std::net::ToSocketAddrs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use types::HTLCStatus;

async fn start_ldk() {
	let args = match args::parse_startup_args() {
		Ok(user_args) => user_args,
		Err(()) => return,
	};

	// Initialize the LDK data directory if necessary.
	let ldk_data_dir = format!("{}/.ldk", args.ldk_storage_dir_path);
	fs::create_dir_all(ldk_data_dir.clone()).unwrap();

	// ## Setup
	// Step 1: Initialize the Logger
	let logger = Arc::new(FilesystemLogger::new(ldk_data_dir.clone()));

	// Initialize our bitcoind client (still needed for block sync even in Esplora mode).
	let mut bitcoind_client_inner = match BitcoindClient::new(
		args.bitcoind_rpc_host.clone(),
		args.bitcoind_rpc_port,
		args.bitcoind_rpc_username.clone(),
		args.bitcoind_rpc_password.clone(),
		args.network,
		tokio::runtime::Handle::current(),
		Arc::clone(&logger),
	)
	.await
	{
		Ok(client) => client,
		Err(e) => {
			println!("Failed to connect to bitcoind client: {}", e);
			return;
		},
	};

	// If BITCOIN_ESPLORA_URL is set, delegate TX broadcast + fees + wallet to Esplora+BDK.
	// Block sync still uses bitcoind (for now). This lets us operate on Signet without
	// relying on bitcoind's wallet.
	if let Ok(esplora_url) = std::env::var("BITCOIN_ESPLORA_URL") {
		let seed_path = std::env::var("BITCOIN_WALLET_SEED_PATH")
			.unwrap_or_else(|_| format!("{}/wallet_seed.txt", ldk_data_dir));
		println!("Esplora backend: {}", esplora_url);
		println!("BDK wallet seed: {}", seed_path);

		let esplora = match crate::esplora::EsploraClient::new(
			esplora_url, Arc::clone(&logger),
		).await {
			Ok(c) => Arc::new(c),
			Err(e) => { println!("ERROR: Esplora init failed: {}", e); return; },
		};

		let bdk = match crate::bdk_wallet::BdkOnchainWallet::new(
			&seed_path, args.network, &esplora.base_url(), Arc::clone(&logger),
		) {
			Ok(w) => Arc::new(w),
			Err(e) => { println!("ERROR: BDK wallet init failed: {}", e); return; },
		};

		// Initial wallet sync
		if let Err(e) = bdk.sync().await {
			println!("WARNING: initial BDK wallet sync failed: {}", e);
		}

		// Start periodic BDK sync (every 30s) + fee update (every 60s)
		let bdk_sync = Arc::clone(&bdk);
		let esplora_fees = Arc::clone(&esplora);
		tokio::spawn(async move {
			let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
			let mut fee_counter = 0u32;
			loop {
				interval.tick().await;
				let _ = bdk_sync.sync().await;
				fee_counter += 1;
				if fee_counter % 2 == 0 {
					esplora_fees.update_fee_estimates().await;
				}
			}
		});

		bitcoind_client_inner.set_esplora_backend(esplora, bdk);
	}

	let bitcoind_client = Arc::new(bitcoind_client_inner);

	// Check that the bitcoind we've connected to is running the network we expect
	let bitcoind_chain = bitcoind_client.get_blockchain_info().await.chain;
	if bitcoind_chain
		!= match args.network {
			bitcoin::Network::Bitcoin => "main",
			bitcoin::Network::Regtest => "regtest",
			bitcoin::Network::Signet => "signet",
			bitcoin::Network::Testnet | _ => "test",
		} {
		println!(
			"Chain argument ({}) didn't match bitcoind chain ({})",
			args.network, bitcoind_chain
		);
		return;
	}

	// Step 2: Initialize the FeeEstimator

	// BitcoindClient implements the FeeEstimator trait, so it'll act as our fee estimator.
	let fee_estimator = bitcoind_client.clone();

	// Step 3: Initialize the BroadcasterInterface

	// BitcoindClient implements the BroadcasterInterface trait, so it'll act as our transaction
	// broadcaster.
	let broadcaster = bitcoind_client.clone();

	// Step 4: Initialize the KeysManager

	// The key seed that we use to derive the node privkey (that corresponds to the node pubkey) and
	// other secret key material.
	let keys_seed_path = format!("{}/keys_seed", ldk_data_dir.clone());
	let keys_seed = if let Ok(seed) = fs::read(keys_seed_path.clone()) {
		assert_eq!(seed.len(), 32);
		let mut key = [0; 32];
		key.copy_from_slice(&seed);
		key
	} else {
		let mut key = [0; 32];
		thread_rng().fill_bytes(&mut key);
		match File::create(keys_seed_path.clone()) {
			Ok(mut f) => {
				Write::write_all(&mut f, &key)
					.expect("Failed to write node keys seed to disk");
				f.sync_all().expect("Failed to sync node keys seed to disk");
			},
			Err(e) => {
				println!("ERROR: Unable to create keys seed file {}: {}", keys_seed_path, e);
				return;
			},
		}
		key
	};
	let cur = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
	let keys_manager =
		Arc::new(KeysManager::new(&keys_seed, cur.as_secs(), cur.subsec_nanos(), true));

	let bump_tx_event_handler = Arc::new(BumpTransactionEventHandler::new(
		Arc::clone(&broadcaster),
		Arc::new(Wallet::new(Arc::clone(&bitcoind_client), Arc::clone(&logger))),
		Arc::clone(&keys_manager),
		Arc::clone(&logger),
	));

	// Step 5: Initialize Persistence
	let fs_store = Arc::new(FilesystemStore::new(ldk_data_dir.clone().into()));
	let persister = MonitorUpdatingPersisterAsync::new(
		Arc::clone(&fs_store),
		TokioSpawner,
		Arc::clone(&logger),
		1000,
		Arc::clone(&keys_manager),
		Arc::clone(&keys_manager),
		Arc::clone(&bitcoind_client),
		Arc::clone(&bitcoind_client),
	);
	// Alternatively, you can use the `FilesystemStore` as a `Persist` directly, at the cost of
	// larger `ChannelMonitor` update writes (but no deletion or cleanup):
	//let persister = Arc::clone(&fs_store);

	// Step 6: Read ChannelMonitor state from disk
	let mut channelmonitors = persister.read_all_channel_monitors_with_updates().await.unwrap();
	// If you are using the `FilesystemStore` as a `Persist` directly, use
	// `lightning::util::persist::read_channel_monitors` like this:
	// read_channel_monitors(Arc::clone(&persister), Arc::clone(&keys_manager), Arc::clone(&keys_manager)).unwrap();

	// Step 7: Initialize the ChainMonitor
	let chain_monitor: Arc<ChainMonitor> = Arc::new(chainmonitor::ChainMonitor::new_async_beta(
		None,
		Arc::clone(&broadcaster),
		Arc::clone(&logger),
		Arc::clone(&fee_estimator),
		persister,
		Arc::clone(&keys_manager),
		keys_manager.get_peer_storage_key(),
	));

	// Step 8: Poll for the best chain tip, which may be used by the channel manager & spv client
	let polled_chain_tip = init::validate_best_block_header(bitcoind_client.as_ref())
		.await
		.expect("Failed to fetch best block header and best block");

	// Step 9: Initialize routing ProbabilisticScorer
	let network_graph_path = format!("{}/network_graph", ldk_data_dir.clone());
	let network_graph =
		Arc::new(disk::read_network(Path::new(&network_graph_path), args.network, logger.clone()));

	let scorer_path = format!("{}/scorer", ldk_data_dir.clone());
	let scorer = Arc::new(RwLock::new(disk::read_scorer(
		Path::new(&scorer_path),
		Arc::clone(&network_graph),
		Arc::clone(&logger),
	)));

	// Step 10: Create Routers
	let scoring_fee_params = ProbabilisticScoringFeeParameters::default();
	let router = Arc::new(DefaultRouter::new(
		network_graph.clone(),
		logger.clone(),
		keys_manager.clone(),
		scorer.clone(),
		scoring_fee_params,
	));

	let message_router = Arc::new(
		lightning::onion_message::messenger::DefaultMessageRouter::new(
			Arc::clone(&network_graph),
			Arc::clone(&keys_manager),
		),
	);

	// Step 11: Initialize the ChannelManager
	let mut user_config = UserConfig::default();
	user_config.channel_handshake_limits.force_announced_channel_preference = false;
	user_config.channel_handshake_config.negotiate_anchors_zero_fee_htlc_tx = true;
	user_config.manually_accept_inbound_channels = true;
	// For testing on Signet: accept channels after 1 confirmation instead of 6.
	let min_depth: u32 = std::env::var("LDK_MIN_CHANNEL_CONFIRMATIONS")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(6);
	if min_depth != 6 {
		user_config.channel_handshake_config.minimum_depth = min_depth;
		println!("Channel minimum confirmations: {}", min_depth);
	}
	let mut restarting_node = true;
	let (channel_manager_blockhash, channel_manager) = {
		if let Ok(f) = fs::File::open(format!("{}/manager", ldk_data_dir.clone())) {
			let mut channel_monitor_references = Vec::new();
			for (_, channel_monitor) in channelmonitors.iter() {
				channel_monitor_references.push(channel_monitor);
			}
			let read_args = ChannelManagerReadArgs::new(
				keys_manager.clone(),
				keys_manager.clone(),
				keys_manager.clone(),
				fee_estimator.clone(),
				chain_monitor.clone(),
				broadcaster.clone(),
				router,
				Arc::clone(&message_router),
				logger.clone(),
				user_config,
				channel_monitor_references,
			);
			<(BlockHash, ChannelManager)>::read(&mut BufReader::new(f), read_args).unwrap()
		} else {
			// We're starting a fresh node.
			restarting_node = false;

			let polled_best_block = polled_chain_tip.to_best_block();
			let polled_best_block_hash = polled_best_block.block_hash;
			let chain_params =
				ChainParameters { network: args.network, best_block: polled_best_block };
			let fresh_channel_manager = channelmanager::ChannelManager::new(
				fee_estimator.clone(),
				chain_monitor.clone(),
				broadcaster.clone(),
				router,
				Arc::clone(&message_router),
				logger.clone(),
				keys_manager.clone(),
				keys_manager.clone(),
				keys_manager.clone(),
				user_config,
				chain_params,
				cur.as_secs() as u32,
			);
			(polled_best_block_hash, fresh_channel_manager)
		}
	};

	// Step 12: Initialize the OutputSweeper.
	let (sweeper_best_block, output_sweeper) = match fs_store
		.read(
			OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE,
			OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
			OUTPUT_SWEEPER_PERSISTENCE_KEY,
		)
		.await
	{
		Err(e) if e.kind() == io::ErrorKind::NotFound => {
			let sweeper = OutputSweeper::new(
				channel_manager.current_best_block(),
				broadcaster.clone(),
				fee_estimator.clone(),
				None,
				keys_manager.clone(),
				bitcoind_client.clone(),
				fs_store.clone(),
				logger.clone(),
			);
			(channel_manager.current_best_block(), sweeper)
		},
		Ok(mut bytes) => {
			let read_args = (
				broadcaster.clone(),
				fee_estimator.clone(),
				None,
				keys_manager.clone(),
				bitcoind_client.clone(),
				fs_store.clone(),
				logger.clone(),
			);
			let mut reader = io::Cursor::new(&mut bytes);
			<(BestBlock, OutputSweeper)>::read(&mut reader, read_args)
				.expect("Failed to deserialize OutputSweeper")
		},
		Err(e) => panic!("Failed to read OutputSweeper with {}", e),
	};

	// Step 13: Sync ChannelMonitors, ChannelManager and OutputSweeper to chain tip
	let mut chain_listener_channel_monitors = Vec::new();
	let mut cache = UnboundedCache::new();
	let chain_tip = if restarting_node {
		let mut chain_listeners = vec![
			(channel_manager_blockhash, &channel_manager as &(dyn chain::Listen + Send + Sync)),
			(sweeper_best_block.block_hash, &output_sweeper as &(dyn chain::Listen + Send + Sync)),
		];

		for (blockhash, channel_monitor) in channelmonitors.drain(..) {
			let funding_txo = channel_monitor.get_funding_txo();
			chain_listener_channel_monitors.push((
				blockhash,
				(channel_monitor, broadcaster.clone(), fee_estimator.clone(), logger.clone()),
				funding_txo,
			));
		}

		for monitor_listener_info in chain_listener_channel_monitors.iter_mut() {
			chain_listeners.push((
				monitor_listener_info.0,
				&monitor_listener_info.1 as &(dyn chain::Listen + Send + Sync),
			));
		}

		init::synchronize_listeners(
			bitcoind_client.as_ref(),
			args.network,
			&mut cache,
			chain_listeners,
		)
		.await
		.unwrap()
	} else {
		polled_chain_tip
	};

	// Step 14: Give ChannelMonitors to ChainMonitor
	for (_, (channel_monitor, _, _, _), _) in chain_listener_channel_monitors {
		let channel_id = channel_monitor.channel_id();
		// Note that this may not return `Completed` for ChannelMonitors which were last written by
		// a version of LDK prior to 0.1.
		assert_eq!(
			chain_monitor.load_existing_monitor(channel_id, channel_monitor),
			Ok(ChannelMonitorUpdateStatus::Completed)
		);
	}

	// Step 15: Optional: Initialize the P2PGossipSync
	let gossip_sync =
		Arc::new(P2PGossipSync::new(Arc::clone(&network_graph), None, Arc::clone(&logger)));

	// Step 16 an OMDomainResolver as a service to other nodes
	// As a service to other LDK users, using an `OMDomainResolver` allows others to resolve BIP
	// 353 Human Readable Names for others, providing them DNSSEC proofs over lightning onion
	// messages. Doing this only makes sense for a always-online public routing node, and doesn't
	// provide you any direct value, but its nice to offer the service for others.
	let channel_manager: Arc<ChannelManager> = Arc::new(channel_manager);
	let resolver = "8.8.8.8:53".to_socket_addrs()
		.expect("failed to resolve DNS address 8.8.8.8:53")
		.next()
		.expect("no socket address resolved for 8.8.8.8:53");
	let domain_resolver =
		Arc::new(OMDomainResolver::new(resolver, Some(Arc::clone(&channel_manager))));

	// Step 17: Initialize the PeerManager
	let onion_messenger: Arc<OnionMessenger> = Arc::new(LdkOnionMessenger::new(
		Arc::clone(&keys_manager),
		Arc::clone(&keys_manager),
		Arc::clone(&logger),
		Arc::clone(&channel_manager),
		Arc::clone(&message_router),
		Arc::clone(&channel_manager),
		Arc::clone(&channel_manager),
		domain_resolver,
		IgnoringMessageHandler {},
	));
	let mut ephemeral_bytes = [0; 32];
	let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
	rand::thread_rng().fill_bytes(&mut ephemeral_bytes);
	let lightning_msg_handler = MessageHandler {
		chan_handler: Arc::clone(&channel_manager),
		route_handler: Arc::clone(&gossip_sync),
		onion_message_handler: Arc::clone(&onion_messenger),
		custom_message_handler: IgnoringMessageHandler {},
		send_only_message_handler: Arc::clone(&chain_monitor),
	};
	let peer_manager: Arc<PeerManager> = Arc::new(PeerManager::new(
		lightning_msg_handler,
		current_time.try_into().unwrap(),
		&ephemeral_bytes,
		logger.clone(),
		Arc::clone(&keys_manager),
	));

	// Install a GossipVerifier in in the P2PGossipSync
	let utxo_lookup = GossipVerifier::new(
		Arc::clone(&bitcoind_client.bitcoind_rpc_client),
		TokioSpawner,
		Arc::clone(&gossip_sync),
		Arc::clone(&peer_manager),
	);
	gossip_sync.add_utxo_lookup(Some(Arc::new(utxo_lookup)));

	// ## Running LDK
	// Step 18: Initialize networking

	let peer_manager_connection_handler = peer_manager.clone();
	let listening_port = args.ldk_peer_listening_port;
	let stop_listen_connect = Arc::new(AtomicBool::new(false));
	let stop_listen = Arc::clone(&stop_listen_connect);
	tokio::spawn(async move {
		let listener = tokio::net::TcpListener::bind(format!("[::]:{}", listening_port))
			.await
			.expect("Failed to bind to listen port - is something else already listening on it?");
		loop {
			let peer_mgr = peer_manager_connection_handler.clone();
			let tcp_stream = match listener.accept().await {
				Ok((stream, _)) => stream,
				Err(e) => {
					println!("WARNING: failed to accept incoming connection: {}", e);
					continue;
				},
			};
			if stop_listen.load(Ordering::Acquire) {
				return;
			}
			if let Ok(std_stream) = tcp_stream.into_std() {
				tokio::spawn(async move {
					lightning_net_tokio::setup_inbound(
						peer_mgr.clone(),
						std_stream,
					)
					.await;
				});
			} else {
				println!("WARNING: failed to convert TCP stream to std");
			}
		}
	});

	// Step 19: Connect and Disconnect Blocks
	let output_sweeper: Arc<OutputSweeper> = Arc::new(output_sweeper);
	let channel_manager_listener = channel_manager.clone();
	let chain_monitor_listener = chain_monitor.clone();
	let output_sweeper_listener = output_sweeper.clone();
	let bitcoind_block_source = bitcoind_client.clone();
	let network = args.network;
	tokio::spawn(async move {
		let chain_poller = poll::ChainPoller::new(bitcoind_block_source.as_ref(), network);
		let chain_listener =
			(chain_monitor_listener, &(channel_manager_listener, output_sweeper_listener));
		let mut spv_client = SpvClient::new(chain_tip, chain_poller, &mut cache, &chain_listener);
		loop {
			if let Err(e) = spv_client.poll_best_tip().await {
				println!("WARNING: chain poll failed: {:?}, retrying in 5s", e);
				tokio::time::sleep(Duration::from_secs(5)).await;
				continue;
			}
			tokio::time::sleep(Duration::from_secs(1)).await;
		}
	});

	let inbound_payments = Arc::new(Mutex::new(disk::read_inbound_payment_info(Path::new(
		&format!("{}/{}", ldk_data_dir, INBOUND_PAYMENTS_FNAME),
	))));
	let outbound_payments = Arc::new(Mutex::new(disk::read_outbound_payment_info(Path::new(
		&format!("{}/{}", ldk_data_dir, OUTBOUND_PAYMENTS_FNAME),
	))));
	let recent_payments_payment_ids = channel_manager
		.list_recent_payments()
		.into_iter()
		.filter_map(|p| match p {
			RecentPaymentDetails::Pending { payment_id, .. } => Some(payment_id),
			RecentPaymentDetails::Fulfilled { payment_id, .. } => Some(payment_id),
			RecentPaymentDetails::Abandoned { payment_id, .. } => Some(payment_id),
			RecentPaymentDetails::AwaitingInvoice { payment_id } => Some(payment_id),
		})
		.collect::<Vec<PaymentId>>();
	for (payment_id, payment_info) in outbound_payments
		.lock()
		.unwrap()
		.payments
		.iter_mut()
		.filter(|(_, i)| matches!(i.status, HTLCStatus::Pending))
	{
		if !recent_payments_payment_ids.contains(payment_id) {
			payment_info.status = HTLCStatus::Failed;
		}
	}
	fs_store
		.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.lock().unwrap().encode())
		.await
		.unwrap();

	// Construct optional Cardano OperatorAgent from config.
	let operator_agent: Option<Arc<cardano_lightning_client::OperatorAgent>> = if let Some(ref cardano_cfg) = args.cardano {
		let skey_raw = std::fs::read_to_string(&cardano_cfg.skey_path)
			.expect("failed to read CARDANO_SKEY_PATH file")
			.trim()
			.to_string();
		// Handle both raw hex and Cardano JSON envelope ({"type":...,"cborHex":"5820..."})
		let skey_hex = if skey_raw.starts_with('{') {
			let v: serde_json::Value =
				serde_json::from_str(&skey_raw).expect("CARDANO_SKEY_PATH: invalid JSON envelope");
			let cbor_hex = v["cborHex"]
				.as_str()
				.expect("CARDANO_SKEY_PATH: JSON missing 'cborHex' field")
				.to_string();
			// Strip CBOR prefix (5820 = 32-byte bytestring tag)
			if cbor_hex.starts_with("5820") {
				cbor_hex[4..].to_string()
			} else {
				cbor_hex
			}
		} else {
			skey_raw
		};
		let script_cbor_raw = std::fs::read_to_string(&cardano_cfg.script_cbor_path)
			.expect("failed to read CARDANO_SCRIPT_CBOR_PATH file")
			.trim()
			.to_string();
		// Aiken's compiledCode is a CBOR byte string (e.g. 590d63...) but CSL's
		// PlutusScript::from_hex_with_version() decodes one CBOR layer, stripping
		// the byte string header. We must double-CBOR-wrap so CSL unwraps to the
		// correct script bytes and computes the right script hash.
		let script_bytes_len = script_cbor_raw.len() / 2; // hex chars -> bytes
		let script_cbor = if script_bytes_len < 24 {
			format!("{:02x}{}", 0x40 + script_bytes_len, script_cbor_raw)
		} else if script_bytes_len < 256 {
			format!("58{:02x}{}", script_bytes_len, script_cbor_raw)
		} else {
			format!("59{:04x}{}", script_bytes_len, script_cbor_raw)
		};

		let cardano_config = cardano_lightning_client::CardanoConfig {
			blockfrost_url: cardano_cfg.blockfrost_url.clone(),
			blockfrost_key: cardano_cfg.blockfrost_key.clone(),
			script_address: cardano_cfg.script_address.clone(),
		};
		let cardano_agent = cardano_lightning_client::CardanoAgent::new(cardano_config);

		let op_config = cardano_lightning_client::OperatorConfig {
			skey_hex,
			operator_address: cardano_cfg.operator_address.clone(),
			operator_pkh: cardano_cfg.operator_pkh.clone(),
			script_cbor,
			cbtc_policy: cardano_cfg.cbtc_policy_id.clone(),
			cbtc_name: cardano_cfg.cbtc_asset_name.clone(),
		};

		let mut agent = cardano_lightning_client::OperatorAgent::new(cardano_agent, op_config);
		agent.init().await.expect("failed to fetch cost models from Blockfrost");
		println!("Cardano operator agent initialized.");
		Some(Arc::new(agent))
	} else {
		None
	};

	// Step 20: Handle LDK Events
	// Create swap database (SQLite) in the LDK storage dir
	let swap_db: Option<Arc<mapping::SwapDb>> = if args.cardano.is_some() {
		let db_path = format!("{}/swaps.db", args.ldk_storage_dir_path);
		Some(Arc::new(mapping::SwapDb::open(&db_path)))
	} else {
		None
	};

	let channel_manager_event_listener = Arc::clone(&channel_manager);
	let bitcoind_client_event_listener = Arc::clone(&bitcoind_client);
	let network_graph_event_listener = Arc::clone(&network_graph);
	let keys_manager_event_listener = Arc::clone(&keys_manager);
	let inbound_payments_event_listener = Arc::clone(&inbound_payments);
	let outbound_payments_event_listener = Arc::clone(&outbound_payments);
	let fs_store_event_listener = Arc::clone(&fs_store);
	let peer_manager_event_listener = Arc::clone(&peer_manager);
	let output_sweeper_event_listener = Arc::clone(&output_sweeper);
	let swap_db_event_listener = swap_db.clone();
	let operator_agent_event_listener = operator_agent.clone();
	let network = args.network;
	let event_handler = move |event: Event| {
		let channel_manager_event_listener = Arc::clone(&channel_manager_event_listener);
		let bitcoind_client_event_listener = Arc::clone(&bitcoind_client_event_listener);
		let network_graph_event_listener = Arc::clone(&network_graph_event_listener);
		let keys_manager_event_listener = Arc::clone(&keys_manager_event_listener);
		let bump_tx_event_handler = Arc::clone(&bump_tx_event_handler);
		let inbound_payments_event_listener = Arc::clone(&inbound_payments_event_listener);
		let outbound_payments_event_listener = Arc::clone(&outbound_payments_event_listener);
		let fs_store_event_listener = Arc::clone(&fs_store_event_listener);
		let peer_manager_event_listener = Arc::clone(&peer_manager_event_listener);
		let output_sweeper_event_listener = Arc::clone(&output_sweeper_event_listener);
		let swap_db_el = swap_db_event_listener.clone();
		let operator_agent_el = operator_agent_event_listener.clone();
		async move {
			events::handle_ldk_events(
				channel_manager_event_listener,
				&bitcoind_client_event_listener,
				&network_graph_event_listener,
				&keys_manager_event_listener,
				&bump_tx_event_handler,
				peer_manager_event_listener,
				inbound_payments_event_listener,
				outbound_payments_event_listener,
				fs_store_event_listener,
				OutputSweeperWrapper(output_sweeper_event_listener),
				network,
				swap_db_el,
				operator_agent_el,
				event,
			)
			.await;
			Ok(())
		}
	};

	// Step 21: Background Processing
	let (bp_exit, bp_exit_check) = tokio::sync::watch::channel(());
	let mut background_processor = tokio::spawn(process_events_async(
		Arc::clone(&fs_store),
		event_handler,
		Arc::clone(&chain_monitor),
		Arc::clone(&channel_manager),
		Some(onion_messenger),
		GossipSync::p2p(Arc::clone(&gossip_sync)),
		Arc::clone(&peer_manager),
		NO_LIQUIDITY_MANAGER,
		Some(Arc::clone(&output_sweeper)),
		Arc::clone(&logger),
		Some(Arc::clone(&scorer)),
		move |t| {
			let mut bp_exit_fut_check = bp_exit_check.clone();
			Box::pin(async move {
				tokio::select! {
					_ = tokio::time::sleep(t) => false,
					_ = bp_exit_fut_check.changed() => true,
				}
			})
		},
		false,
		|| Some(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap()),
	));

	// Background tasks: peer reconnect and node announcement
	let stop_connect = Arc::clone(&stop_listen_connect);
	tokio::spawn(background::reconnect_peers(
		Arc::clone(&channel_manager),
		Arc::clone(&peer_manager),
		stop_connect,
		Arc::clone(&network_graph),
	));

	tokio::spawn(background::broadcast_node_announcement(
		Arc::clone(&peer_manager),
		Arc::clone(&channel_manager),
		args.ldk_announced_node_name,
		args.ldk_announced_listen_addr.clone(),
	));

	tokio::spawn(sweep::migrate_deprecated_spendable_outputs(
		ldk_data_dir.clone(),
		Arc::clone(&keys_manager),
		Arc::clone(&logger),
		Arc::clone(&fs_store),
		Arc::clone(&output_sweeper),
	));

	// Recover stuck swaps/offramps from previous crash
	if let (Some(op), Some(db)) = (&operator_agent, &swap_db) {
		recovery::recover_fulfilling_swaps(op.as_ref(), db).await;
		recovery::recover_depositing_offramps(op.as_ref(), db).await;
	}

	// Reconcile on-chain state: cancel expired invoices/offramps that were
	// orphaned by prior relay restarts (not tracked in SQLite but still on-chain).
	// Env var opt-in because it modifies contract state and takes 30+ seconds per entry.
	if let Some(ref op) = operator_agent {
		if std::env::var("CARDANO_RECONCILE_ON_STARTUP").unwrap_or_default() == "1" {
			println!("Reconciling on-chain state (cancelling orphaned expired entries)...");
			crate::cli::cardano_cmds::cancel_expired(op).await;
			crate::cli::cardano_cmds::cancel_expired_offramps(op).await;
			println!("Reconciliation complete.");
		}
	}

	// Start expiry monitors for Cardano swaps and offramps
	if let (Some(op), Some(db)) = (&operator_agent, &swap_db) {
		tokio::spawn(background::monitor_expired_swaps(
			Arc::clone(op),
			Arc::clone(db),
		));
		tokio::spawn(background::monitor_expired_offramps(
			Arc::clone(op),
			Arc::clone(db),
		));
	}

	// Start REST API server for swap requests (if Cardano is enabled)
	if let (Some(op), Some(db)) = (&operator_agent, &swap_db) {
		let auth_token = std::env::var("CARDANO_API_AUTH_TOKEN").ok();
		if auth_token.is_some() {
			println!("API operator auth enabled (bearer token required for pool deposit/withdraw)");
		} else {
			println!("WARNING: No CARDANO_API_AUTH_TOKEN set — /pool/deposit and /pool/withdraw are UNPROTECTED");
			println!("WARNING: Set CARDANO_API_AUTH_TOKEN to require bearer token authentication");
		}
		let max_active_swaps: i64 = std::env::var("CARDANO_MAX_ACTIVE_SWAPS")
			.unwrap_or_else(|_| "50".into())
			.parse()
			.expect("CARDANO_MAX_ACTIVE_SWAPS must be a number");
		let max_active_offramps: i64 = std::env::var("CARDANO_MAX_ACTIVE_OFFRAMPS")
			.unwrap_or_else(|_| "50".into())
			.parse()
			.expect("CARDANO_MAX_ACTIVE_OFFRAMPS must be a number");
		let swap_expiry_secs: i64 = std::env::var("CARDANO_SWAP_EXPIRY_SECONDS")
			.unwrap_or_else(|_| "3600".into())
			.parse()
			.expect("CARDANO_SWAP_EXPIRY_SECONDS must be a number");
		assert!(swap_expiry_secs > 0, "CARDANO_SWAP_EXPIRY_SECONDS must be positive (got {})", swap_expiry_secs);
		let swap_expiry_ms = swap_expiry_secs * 1000;
		if swap_expiry_secs != 3600 {
			println!("Swap/offramp expiry set to {} seconds", swap_expiry_secs);
		}
		// Rate limit: max requests per IP per window. Production-safe default
		// (10/60s) is too strict for E2E tests and for a frontend that polls
		// status after every action — override via env for dev/test.
		let rate_limit_max: u32 = std::env::var("CARDANO_API_RATE_LIMIT_MAX")
			.unwrap_or_else(|_| "10".into())
			.parse()
			.expect("CARDANO_API_RATE_LIMIT_MAX must be a number");
		let rate_limit_window_secs: u64 = std::env::var("CARDANO_API_RATE_LIMIT_WINDOW_SECS")
			.unwrap_or_else(|_| "60".into())
			.parse()
			.expect("CARDANO_API_RATE_LIMIT_WINDOW_SECS must be a number");
		if rate_limit_max != 10 || rate_limit_window_secs != 60 {
			println!("API rate limit: {} requests per {}s per IP", rate_limit_max, rate_limit_window_secs);
		}
		let api_state = api::ApiState {
			operator: Arc::clone(op),
			swap_db: Arc::clone(db),
			channel_manager: Arc::clone(&channel_manager),
			output_sweeper: Arc::clone(&output_sweeper),
			bitcoind_client: Arc::clone(&bitcoind_client),
			inbound_payments: Arc::clone(&inbound_payments),
			outbound_payments: Arc::clone(&outbound_payments),
			fs_store: Arc::clone(&fs_store),
			auth_token,
			rate_limiter: Arc::new(std::sync::Mutex::new(
				api::RateLimiter::new(rate_limit_max, rate_limit_window_secs),
			)),
			max_active_swaps,
			max_active_offramps,
			swap_expiry_ms,
		};
		let api_port: u16 = std::env::var("CARDANO_API_PORT")
			.unwrap_or_else(|_| "3000".into())
			.parse()
			.expect("CARDANO_API_PORT must be a valid port number");
		let router = api::create_router(api_state);
		let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", api_port))
			.await
			.expect("failed to bind API server");
		println!("Cardano swap API listening on port {}", api_port);
		tokio::spawn(async move {
			if let Err(e) = axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await {
				println!("ERROR: API server exited: {}", e);
			}
		});
	}

	// Start the CLI.
	let cli_channel_manager = Arc::clone(&channel_manager);
	let cli_chain_monitor = Arc::clone(&chain_monitor);
	let cli_fs_store = Arc::clone(&fs_store);
	let cli_peer_manager = Arc::clone(&peer_manager);
	let cli_output_sweeper = Arc::clone(&output_sweeper);
	let cli_bitcoind_client = Arc::clone(&bitcoind_client);
	let cli_poll = tokio::task::spawn(cli::poll_for_user_input(
		cli_peer_manager,
		cli_channel_manager,
		cli_chain_monitor,
		keys_manager,
		network_graph,
		inbound_payments,
		outbound_payments,
		cli_fs_store,
		operator_agent,
		cli_output_sweeper,
		cli_bitcoind_client,
	));

	// Exit if either CLI polling exits or the background processor exits (which shouldn't happen
	// unless we fail to write to the filesystem).
	let mut bg_res = Ok(Ok(()));
	tokio::select! {
		_ = cli_poll => {},
		bg_exit = &mut background_processor => {
			bg_res = bg_exit;
		},
	}

	// Disconnect our peers and stop accepting new connections. This ensures we don't continue
	// updating our channel data after we've stopped the background processor.
	stop_listen_connect.store(true, Ordering::Release);
	peer_manager.disconnect_all_peers();

	if let Err(e) = bg_res {
		let persist_res = fs_store
			.write(
				persist::CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
				persist::CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
				persist::CHANNEL_MANAGER_PERSISTENCE_KEY,
				channel_manager.encode(),
			)
			.await
			.unwrap();
		use lightning::util::logger::Logger;
		lightning::log_error!(
			&*logger,
			"Last-ditch ChannelManager persistence result: {:?}",
			persist_res
		);
		panic!(
			"ERR: background processing stopped with result {:?}, exiting.\n\
			Last-ditch ChannelManager persistence result {:?}",
			e, persist_res
		);
	}

	// Stop the background processor.
	if !bp_exit.is_closed() {
		bp_exit.send(()).unwrap();
		background_processor.await.unwrap().unwrap();
	}
}

#[tokio::main]
pub async fn main() {
	#[cfg(not(target_os = "windows"))]
	{
		// Catch Ctrl-C with a dummy signal handler.
		unsafe {
			let mut new_action: libc::sigaction = core::mem::zeroed();
			let mut old_action: libc::sigaction = core::mem::zeroed();

			extern "C" fn dummy_handler(
				_: libc::c_int, _: *const libc::siginfo_t, _: *const libc::c_void,
			) {
			}

			new_action.sa_sigaction = dummy_handler as libc::sighandler_t;
			new_action.sa_flags = libc::SA_SIGINFO;

			libc::sigaction(
				libc::SIGINT,
				&new_action as *const libc::sigaction,
				&mut old_action as *mut libc::sigaction,
			);
		}
	}

	start_ldk().await;
}
