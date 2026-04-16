pub(crate) mod cardano_cmds;
pub(crate) mod channel_cmds;
pub(crate) mod payment_cmds;
pub(crate) mod peer_cmds;

use crate::disk::{INBOUND_PAYMENTS_FNAME, OUTBOUND_PAYMENTS_FNAME};
use crate::hex_utils;
use crate::types::{
	ChainMonitor, ChannelManager, HTLCStatus, InboundPaymentInfoStorage, MillisatAmount,
	NetworkGraph, OutboundPaymentInfoStorage, OutputSweeper, PaymentInfo, PeerManager,
};
use bitcoin::secp256k1::PublicKey;
use cardano_lightning_client::OperatorAgent;
use lightning::ln::channelmanager::{OptionalOfferPaymentParams, PaymentId, Retry};
use lightning::ln::msgs::SocketAddress;
use lightning::offers::offer::{self, Offer};
use lightning::onion_message::dns_resolution::HumanReadableName;
use lightning::onion_message::messenger::Destination;
use lightning::sign::{EntropySource, KeysManager};
use lightning::util::persist::KVStore;
use lightning::util::ser::Writeable;
use lightning_invoice::Bolt11Invoice;
use lightning_persister::fs_store::FilesystemStore;
use std::env;
use std::io::Write;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

pub(crate) use self::peer_cmds::{connect_peer_if_necessary, do_connect_peer};

use bitcoin::network::Network;

pub(crate) struct LdkUserInfo {
	pub(crate) bitcoind_rpc_username: String,
	pub(crate) bitcoind_rpc_password: String,
	pub(crate) bitcoind_rpc_port: u16,
	pub(crate) bitcoind_rpc_host: String,
	pub(crate) ldk_storage_dir_path: String,
	pub(crate) ldk_peer_listening_port: u16,
	pub(crate) ldk_announced_listen_addr: Vec<SocketAddress>,
	pub(crate) ldk_announced_node_name: [u8; 32],
	pub(crate) network: Network,
	pub(crate) cardano: Option<CardanoRelayConfig>,
}

/// Cardano-specific configuration for the relay, parsed from CARDANO_* env vars.
#[derive(Debug, Clone)]
pub(crate) struct CardanoRelayConfig {
	/// Blockfrost-compatible API base URL.
	pub blockfrost_url: String,
	/// Blockfrost API key.
	pub blockfrost_key: String,
	/// Path to operator signing key (CBOR envelope or raw ed25519).
	pub skey_path: String,
	/// LM contract script address.
	pub script_address: String,
	/// Path to applied Plutus script CBOR hex file.
	pub script_cbor_path: String,
	/// cBTC policy ID (hex).
	pub cbtc_policy_id: String,
	/// cBTC asset name (hex).
	pub cbtc_asset_name: String,
	/// Operator's Cardano address (bech32).
	pub operator_address: String,
	/// Operator's payment key hash (hex).
	pub operator_pkh: String,
}

pub(crate) async fn poll_for_user_input(
	peer_manager: Arc<PeerManager>, channel_manager: Arc<ChannelManager>,
	chain_monitor: Arc<ChainMonitor>, keys_manager: Arc<KeysManager>,
	network_graph: Arc<NetworkGraph>, inbound_payments: Arc<Mutex<InboundPaymentInfoStorage>>,
	outbound_payments: Arc<Mutex<OutboundPaymentInfoStorage>>, fs_store: Arc<FilesystemStore>,
	operator_agent: Option<Arc<OperatorAgent>>, output_sweeper: Arc<OutputSweeper>,
) {
	println!(
		"LDK startup successful. Enter \"help\" to view available commands. Press Ctrl-D to quit."
	);
	println!("LDK logs are available at <your-supplied-ldk-data-dir-path>/.ldk/logs");
	println!("Local Node ID is {}.", channel_manager.get_our_node_id());

	let mut input = BufReader::new(tokio::io::stdin()).lines();
	'read_command: loop {
		print!("> ");
		std::io::stdout().flush().unwrap(); // Without flushing, the `>` doesn't print
		let line = match input.next_line().await {
			Ok(Some(l)) => l,
			Err(e) => {
				break println!("ERROR: {}", e);
			},
			Ok(None) => {
				break println!("ERROR: End of stdin");
			},
		};

		let mut words = line.split_whitespace();
		if let Some(word) = words.next() {
			match word {
				"help" => help(),
				"openchannel" => {
					let peer_pubkey_and_ip_addr = words.next();
					let channel_value_sat = words.next();
					if peer_pubkey_and_ip_addr.is_none() || channel_value_sat.is_none() {
						println!("ERROR: openchannel has 2 required arguments: `openchannel pubkey@host:port channel_amt_satoshis` [--public] [--with-anchors]");
						continue;
					}
					let peer_pubkey_and_ip_addr = peer_pubkey_and_ip_addr.unwrap();

					let mut pubkey_and_addr = peer_pubkey_and_ip_addr.split("@");
					let pubkey = pubkey_and_addr.next();
					let peer_addr_str = pubkey_and_addr.next();
					let pubkey = hex_utils::to_compressed_pubkey(pubkey.unwrap());
					if pubkey.is_none() {
						println!("ERROR: unable to parse given pubkey for node");
						continue;
					}
					let pubkey = pubkey.unwrap();

					if peer_addr_str.is_none() {
						if peer_manager.peer_by_node_id(&pubkey).is_none() {
							println!("ERROR: Peer address not provided and peer is not connected");
							continue;
						}
					} else {
						let (pubkey, peer_addr) =
							match peer_cmds::parse_peer_info(peer_pubkey_and_ip_addr.to_string()) {
								Ok(info) => info,
								Err(e) => {
									println!("{:?}", e.into_inner().unwrap());
									continue;
								},
							};

						if peer_cmds::connect_peer_if_necessary(pubkey, peer_addr, peer_manager.clone())
							.await
							.is_err()
						{
							continue;
						};
					}

					let chan_amt_sat: Result<u64, _> = channel_value_sat.unwrap().parse();
					if chan_amt_sat.is_err() {
						println!("ERROR: channel amount must be a number");
						continue;
					}
					let (mut announce_channel, mut with_anchors) = (false, false);
					while let Some(word) = words.next() {
						match word {
							"--public" | "--public=true" => announce_channel = true,
							"--public=false" => announce_channel = false,
							"--with-anchors" | "--with-anchors=true" => with_anchors = true,
							"--with-anchors=false" => with_anchors = false,
							_ => {
								println!("ERROR: invalid boolean flag format. Valid formats: `--option`, `--option=true` `--option=false`");
								continue;
							},
						}
					}

					let _ = channel_cmds::open_channel(
						pubkey,
						chan_amt_sat.unwrap(),
						announce_channel,
						with_anchors,
						channel_manager.clone(),
					);
				},
				"sendpayment" => {
					let invoice_str = words.next();
					if invoice_str.is_none() {
						println!("ERROR: sendpayment requires an invoice: `sendpayment <invoice> [amount_msat]`");
						continue;
					}
					let invoice_str = invoice_str.unwrap();

					let mut user_provided_amt: Option<u64> = None;
					if let Some(amt_msat_str) = words.next() {
						match amt_msat_str.parse() {
							Ok(amt) => user_provided_amt = Some(amt),
							Err(e) => {
								println!("ERROR: couldn't parse amount_msat: {}", e);
								continue;
							},
						};
					}

					if let Ok(offer) = Offer::from_str(invoice_str) {
						let random_bytes = keys_manager.get_secure_random_bytes();
						let payment_id = PaymentId(random_bytes);

						let amt_msat = match (offer.amount(), user_provided_amt) {
							(Some(offer::Amount::Bitcoin { amount_msats }), _) => amount_msats,
							(_, Some(amt)) => amt,
							(amt, _) => {
								println!("ERROR: Cannot process non-Bitcoin-denominated offer value {:?}", amt);
								continue;
							},
						};
						if user_provided_amt.is_some() && user_provided_amt != Some(amt_msat) {
							println!("Amount didn't match offer of {}msat", amt_msat);
							continue;
						}

						while user_provided_amt.is_none() {
							print!("Paying offer for {} msat. Continue (Y/N)? >", amt_msat);
							std::io::stdout().flush().unwrap();

							let line = match input.next_line().await {
								Ok(Some(l)) => l,
								Err(e) => {
									println!("ERROR: {}", e);
									break 'read_command;
								},
								Ok(None) => {
									println!("ERROR: End of stdin");
									break 'read_command;
								},
							};

							if line.starts_with("Y") {
								break;
							}
							if line.starts_with("N") {
								continue 'read_command;
							}
						}

						outbound_payments.lock().unwrap().payments.insert(
							payment_id,
							PaymentInfo {
								preimage: None,
								secret: None,
								status: HTLCStatus::Pending,
								amt_msat: MillisatAmount(Some(amt_msat)),
							},
						);
						fs_store
							.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
							.await
							.unwrap();

						let params = OptionalOfferPaymentParams {
							retry_strategy: Retry::Timeout(Duration::from_secs(10)),
							..Default::default()
						};
						let amt = Some(amt_msat);
						let pay = channel_manager.pay_for_offer(&offer, amt, payment_id, params);
						if pay.is_ok() {
							println!("Payment in flight");
						} else {
							println!("ERROR: Failed to pay: {:?}", pay);
						}
					} else if let Ok(hrn) = HumanReadableName::from_encoded(invoice_str) {
						let random_bytes = keys_manager.get_secure_random_bytes();
						let payment_id = PaymentId(random_bytes);

						if user_provided_amt.is_none() {
							println!("Can't pay to a human-readable-name without an amount");
							continue;
						}

						// We need some nodes that will resolve DNS for us in order to pay a Human
						// Readable Name. They don't need to be trusted, but until onion message
						// forwarding is widespread we'll directly connect to them, revealing who
						// we intend to pay.
						let mut dns_resolvers = Vec::new();
						for (node_id, node) in network_graph.read_only().nodes().unordered_iter() {
							if let Some(info) = &node.announcement_info {
								// Sadly, 31 nodes currently squat on the DNS Resolver feature bit
								// without speaking it.
								// Its unclear why they're doing so, but none of them currently
								// also have the onion messaging feature bit set, so here we check
								// for both.
								let supports_dns = info.features().supports_dns_resolution();
								let supports_om = info.features().supports_onion_messages();
								if supports_dns && supports_om {
									if let Ok(pubkey) = node_id.as_pubkey() {
										dns_resolvers.push(Destination::Node(pubkey));
									}
								}
							}
							if dns_resolvers.len() > 5 {
								break;
							}
						}
						if dns_resolvers.is_empty() {
							println!(
								"Failed to find any DNS resolving nodes, check your network graph is synced"
							);
							continue;
						}

						let amt_msat = user_provided_amt.unwrap();
						outbound_payments.lock().unwrap().payments.insert(
							payment_id,
							PaymentInfo {
								preimage: None,
								secret: None,
								status: HTLCStatus::Pending,
								amt_msat: MillisatAmount(Some(amt_msat)),
							},
						);
						fs_store
							.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
							.await
							.unwrap();

						let params = OptionalOfferPaymentParams {
							retry_strategy: Retry::Timeout(Duration::from_secs(10)),
							..Default::default()
						};
						let pay = |a, b, c, d, e| {
							channel_manager.pay_for_offer_from_human_readable_name(a, b, c, d, e)
						};
						let pay = pay(hrn, amt_msat, payment_id, params, dns_resolvers);
						if pay.is_ok() {
							println!("Payment in flight");
						} else {
							println!("ERROR: Failed to pay");
						}
					} else {
						match Bolt11Invoice::from_str(invoice_str) {
							Ok(invoice) => {
								let _ = payment_cmds::send_payment(
									&channel_manager,
									&invoice,
									user_provided_amt,
									&outbound_payments,
									&*fs_store,
								)
								.await;
							},
							Err(e) => {
								println!("ERROR: invalid invoice: {:?}", e);
							},
						}
					}
				},
				"keysend" => {
					let dest_pubkey = match words.next() {
						Some(dest) => match hex_utils::to_compressed_pubkey(dest) {
							Some(pk) => pk,
							None => {
								println!("ERROR: couldn't parse destination pubkey");
								continue;
							},
						},
						None => {
							println!("ERROR: keysend requires a destination pubkey: `keysend <dest_pubkey> <amt_msat>`");
							continue;
						},
					};
					let amt_msat_str = match words.next() {
						Some(amt) => amt,
						None => {
							println!("ERROR: keysend requires an amount in millisatoshis: `keysend <dest_pubkey> <amt_msat>`");
							continue;
						},
					};
					let amt_msat: u64 = match amt_msat_str.parse() {
						Ok(amt) => amt,
						Err(e) => {
							println!("ERROR: couldn't parse amount_msat: {}", e);
							continue;
						},
					};
					payment_cmds::keysend(
						&channel_manager,
						dest_pubkey,
						amt_msat,
						&*keys_manager,
						&outbound_payments,
						&*fs_store,
					)
					.await;
				},
				"getoffer" => {
					let offer_builder = channel_manager.create_offer_builder();
					if let Err(e) = offer_builder {
						println!("ERROR: Failed to initiate offer building: {:?}", e);
						continue;
					}

					let amt_str = words.next();
					let offer = if amt_str.is_some() {
						let amt_msat: Result<u64, _> = amt_str.unwrap().parse();
						if amt_msat.is_err() {
							println!("ERROR: getoffer provided payment amount was not a number");
							continue;
						}
						offer_builder.unwrap().amount_msats(amt_msat.unwrap()).build()
					} else {
						offer_builder.unwrap().build()
					};

					if offer.is_err() {
						println!("ERROR: Failed to build offer: {:?}", offer.unwrap_err());
					} else {
						// Note that unlike BOLT11 invoice creation we don't bother to add a
						// pending inbound payment here, as offers can be reused and don't
						// correspond with individual payments.
						println!("{}", offer.unwrap());
					}
				},
				"getinvoice" => {
					let amt_str = words.next();
					if amt_str.is_none() {
						println!("ERROR: getinvoice requires an amount in millisatoshis");
						continue;
					}

					let amt_msat: Result<u64, _> = amt_str.unwrap().parse();
					if amt_msat.is_err() {
						println!("ERROR: getinvoice provided payment amount was not a number");
						continue;
					}

					let expiry_secs_str = words.next();
					if expiry_secs_str.is_none() {
						println!("ERROR: getinvoice requires an expiry in seconds");
						continue;
					}

					let expiry_secs: Result<u32, _> = expiry_secs_str.unwrap().parse();
					if expiry_secs.is_err() {
						println!("ERROR: getinvoice provided expiry was not a number");
						continue;
					}

					let write_future = {
						let mut inbound_payments = inbound_payments.lock().unwrap();
						payment_cmds::get_invoice(
							amt_msat.unwrap(),
							&mut inbound_payments,
							&channel_manager,
							expiry_secs.unwrap(),
						);
						fs_store.write("", "", INBOUND_PAYMENTS_FNAME, inbound_payments.encode())
					};
					write_future.await.unwrap();
				},
				"connectpeer" => {
					let peer_pubkey_and_ip_addr = words.next();
					if peer_pubkey_and_ip_addr.is_none() {
						println!("ERROR: connectpeer requires peer connection info: `connectpeer pubkey@host:port`");
						continue;
					}
					let (pubkey, peer_addr) =
						match peer_cmds::parse_peer_info(peer_pubkey_and_ip_addr.unwrap().to_string()) {
							Ok(info) => info,
							Err(e) => {
								println!("{:?}", e.into_inner().unwrap());
								continue;
							},
						};
					if peer_cmds::connect_peer_if_necessary(pubkey, peer_addr, peer_manager.clone())
						.await
						.is_ok()
					{
						println!("SUCCESS: connected to peer {}", pubkey);
					}
				},
				"disconnectpeer" => {
					let peer_pubkey = words.next();
					if peer_pubkey.is_none() {
						println!("ERROR: disconnectpeer requires peer public key: `disconnectpeer <peer_pubkey>`");
						continue;
					}

					let peer_pubkey =
						match PublicKey::from_str(peer_pubkey.unwrap()) {
							Ok(pubkey) => pubkey,
							Err(e) => {
								println!("ERROR: {}", e.to_string());
								continue;
							},
						};

					if peer_cmds::do_disconnect_peer(
						peer_pubkey,
						peer_manager.clone(),
						channel_manager.clone(),
					)
					.is_ok()
					{
						println!("SUCCESS: disconnected from peer {}", peer_pubkey);
					}
				},
				"listchannels" => channel_cmds::list_channels(&channel_manager, &network_graph),
				"listpayments" => payment_cmds::list_payments(
					&inbound_payments.lock().unwrap(),
					&outbound_payments.lock().unwrap(),
				),
				"closechannel" => {
					let channel_id_str = words.next();
					if channel_id_str.is_none() {
						println!("ERROR: closechannel requires a channel ID: `closechannel <channel_id> <peer_pubkey>`");
						continue;
					}
					let channel_id_vec = hex_utils::to_vec(channel_id_str.unwrap());
					if channel_id_vec.is_none() || channel_id_vec.as_ref().unwrap().len() != 32 {
						println!("ERROR: couldn't parse channel_id");
						continue;
					}
					let mut channel_id = [0; 32];
					channel_id.copy_from_slice(&channel_id_vec.unwrap());

					let peer_pubkey_str = words.next();
					if peer_pubkey_str.is_none() {
						println!("ERROR: closechannel requires a peer pubkey: `closechannel <channel_id> <peer_pubkey>`");
						continue;
					}
					let peer_pubkey_vec = match hex_utils::to_vec(peer_pubkey_str.unwrap()) {
						Some(peer_pubkey_vec) => peer_pubkey_vec,
						None => {
							println!("ERROR: couldn't parse peer_pubkey");
							continue;
						},
					};
					let peer_pubkey = match PublicKey::from_slice(&peer_pubkey_vec) {
						Ok(peer_pubkey) => peer_pubkey,
						Err(_) => {
							println!("ERROR: couldn't parse peer_pubkey");
							continue;
						},
					};

					channel_cmds::close_channel(channel_id, peer_pubkey, channel_manager.clone());
				},
				"forceclosechannel" => {
					let channel_id_str = words.next();
					if channel_id_str.is_none() {
						println!("ERROR: forceclosechannel requires a channel ID: `forceclosechannel <channel_id> <peer_pubkey>`");
						continue;
					}
					let channel_id_vec = hex_utils::to_vec(channel_id_str.unwrap());
					if channel_id_vec.is_none() || channel_id_vec.as_ref().unwrap().len() != 32 {
						println!("ERROR: couldn't parse channel_id");
						continue;
					}
					let mut channel_id = [0; 32];
					channel_id.copy_from_slice(&channel_id_vec.unwrap());

					let peer_pubkey_str = words.next();
					if peer_pubkey_str.is_none() {
						println!("ERROR: forceclosechannel requires a peer pubkey: `forceclosechannel <channel_id> <peer_pubkey>`");
						continue;
					}
					let peer_pubkey_vec = match hex_utils::to_vec(peer_pubkey_str.unwrap()) {
						Some(peer_pubkey_vec) => peer_pubkey_vec,
						None => {
							println!("ERROR: couldn't parse peer_pubkey");
							continue;
						},
					};
					let peer_pubkey = match PublicKey::from_slice(&peer_pubkey_vec) {
						Ok(peer_pubkey) => peer_pubkey,
						Err(_) => {
							println!("ERROR: couldn't parse peer_pubkey");
							continue;
						},
					};

					channel_cmds::force_close_channel(channel_id, peer_pubkey, channel_manager.clone());
				},
				"nodeinfo" => {
					peer_cmds::node_info(&channel_manager, &chain_monitor, &peer_manager, &network_graph)
				},
				"listpeers" => peer_cmds::list_peers(peer_manager.clone()),
				"signmessage" => {
					const MSG_STARTPOS: usize = "signmessage".len() + 1;
					if line.trim().as_bytes().len() <= MSG_STARTPOS {
						println!("ERROR: signmsg requires a message");
						continue;
					}
					println!(
						"{:?}",
						lightning::util::message_signing::sign(
							&line.trim().as_bytes()[MSG_STARTPOS..],
							&keys_manager.get_node_secret_key()
						)
					);
				},
				"pool-info" => {
					if let Some(ref op) = operator_agent {
						cardano_cmds::pool_info(op).await;
					} else {
						println!("ERROR: Cardano not enabled. Set CARDANO_ENABLED=true with required env vars.");
					}
				},
				"getbalance" => {
					print_balance(&channel_manager, &output_sweeper, operator_agent.as_ref()).await;
				},
				"cardano-deposit" => {
					if let Some(ref op) = operator_agent {
						let amount_str = words.next();
						if amount_str.is_none() {
							println!("ERROR: cardano-deposit requires an amount: `cardano-deposit <cbtc_amount>`");
							continue;
						}
						let amount: i64 = match amount_str.unwrap().parse() {
							Ok(a) => a,
							Err(_) => {
								println!("ERROR: amount must be a positive integer");
								continue;
							},
						};
						cardano_cmds::cardano_deposit(op, amount).await;
					} else {
						println!("ERROR: Cardano not enabled. Set CARDANO_ENABLED=true with required env vars.");
					}
				},
				"cardano-withdraw" => {
					if let Some(ref op) = operator_agent {
						let amount_str = words.next();
						if amount_str.is_none() {
							println!("ERROR: cardano-withdraw requires an amount: `cardano-withdraw <cbtc_amount>`");
							continue;
						}
						let amount: i64 = match amount_str.unwrap().parse() {
							Ok(a) => a,
							Err(_) => {
								println!("ERROR: amount must be a positive integer");
								continue;
							},
						};
						cardano_cmds::cardano_withdraw(op, amount).await;
					} else {
						println!("ERROR: Cardano not enabled. Set CARDANO_ENABLED=true with required env vars.");
					}
				},
				"cancel-expired" => {
					if let Some(ref op) = operator_agent {
						cardano_cmds::cancel_expired(op).await;
					} else {
						println!("ERROR: Cardano not enabled. Set CARDANO_ENABLED=true with required env vars.");
					}
				},
				"cancel-expired-offramps" => {
					if let Some(ref op) = operator_agent {
						cardano_cmds::cancel_expired_offramps(op).await;
					} else {
						println!("ERROR: Cardano not enabled. Set CARDANO_ENABLED=true with required env vars.");
					}
				},
				"quit" | "exit" => break,
				_ => println!("Unknown command. See `\"help\" for available commands."),
			}
		}
	}
}

fn help() {
	let package_version = env!("CARGO_PKG_VERSION");
	let package_name = env!("CARGO_PKG_NAME");
	println!("\nVERSION:");
	println!("  {} v{}", package_name, package_version);
	println!("\nUSAGE:");
	println!("  Command [arguments]");
	println!("\nCOMMANDS:");
	println!("  help\tShows a list of commands.");
	println!("  quit\tClose the application.");
	println!("\n  Channels:");
	println!("      openchannel pubkey@[host:port] <amt_satoshis> [--public] [--with-anchors]");
	println!("      closechannel <channel_id> <peer_pubkey>");
	println!("      forceclosechannel <channel_id> <peer_pubkey>");
	println!("      listchannels");
	println!("\n  Peers:");
	println!("      connectpeer pubkey@host:port");
	println!("      disconnectpeer <peer_pubkey>");
	println!("      listpeers");
	println!("\n  Payments:");
	println!("      sendpayment <invoice|offer|human readable name> [<amount_msat>]");
	println!("      keysend <dest_pubkey> <amt_msats>");
	println!("      listpayments");
	println!("\n  Invoices:");
	println!("      getinvoice <amt_msats> <expiry_secs>");
	println!("      getoffer [<amt_msats>]");
	println!("\n  Cardano (requires CARDANO_ENABLED=true):");
	println!("      pool-info");
	println!("      getbalance  (BTC + cBTC balances held by this relay)");
	println!("      cardano-deposit <cbtc_amount>");
	println!("      cardano-withdraw <cbtc_amount>");
	println!("      cancel-expired");
	println!("      cancel-expired-offramps");
	println!("\n  Other:");
	println!("      signmessage <message>");
	println!("      nodeinfo");
}

/// Print a summary of all BTC and cBTC balances held by this relay.
///
/// BTC side:
///   - Channel balances (outbound + inbound per channel)
///   - Post-close on-chain BTC tracked by OutputSweeper
///
/// cBTC side:
///   - On-chain contract pool state (if CARDANO_ENABLED)
async fn print_balance(
	channel_manager: &Arc<ChannelManager>, output_sweeper: &Arc<OutputSweeper>,
	operator_agent: Option<&Arc<OperatorAgent>>,
) {
	println!("Balance:");
	println!("  BTC (Lightning side):");
	let channels = channel_manager.list_channels();
	let mut total_out: u64 = 0;
	let mut total_in: u64 = 0;
	if channels.is_empty() {
		println!("    No open channels");
	} else {
		for c in &channels {
			total_out += c.outbound_capacity_msat;
			total_in += c.inbound_capacity_msat;
			println!("    Channel {}:", c.channel_id);
			println!("      peer:              {}", c.counterparty.node_id);
			println!("      ready:             {}", c.is_channel_ready);
			println!("      capacity:          {} sats", c.channel_value_satoshis);
			println!("      outbound:          {} msat", c.outbound_capacity_msat);
			println!("      inbound:           {} msat", c.inbound_capacity_msat);
		}
		println!("    Total outbound:      {} msat ({} sats)", total_out, total_out / 1000);
		println!("    Total inbound:       {} msat ({} sats)", total_in, total_in / 1000);
	}

	// Post-close on-chain BTC tracked by the sweeper (broken down by spend status).
	let tracked = output_sweeper.tracked_spendable_outputs();
	let (mut pending, mut confirmed) = (0u64, 0u64);
	for o in &tracked {
		use lightning::sign::SpendableOutputDescriptor;
		let v = match &o.descriptor {
			SpendableOutputDescriptor::StaticOutput { output, .. } => output.value.to_sat(),
			SpendableOutputDescriptor::DelayedPaymentOutput(d) => d.output.value.to_sat(),
			SpendableOutputDescriptor::StaticPaymentOutput(s) => s.output.value.to_sat(),
		};
		use lightning::util::sweep::OutputSpendStatus;
		match o.status {
			OutputSpendStatus::PendingThresholdConfirmations { .. } => confirmed += v,
			_ => pending += v,
		}
	}
	println!("  BTC (on-chain, post-close sweeper):");
	println!("    tracked outputs:     {}", tracked.len());
	println!("    pending sats:        {} (spend TX not yet confirmed)", pending);
	println!("    confirmed sats:      {} (swept, awaiting prune delay)", confirmed);
	println!("    total sats:          {}", pending + confirmed);

	// cBTC contract state.
	println!("  cBTC (Cardano contract pool):");
	match operator_agent {
		Some(op) => match op.agent().query_state().await {
			Ok(s) => {
				println!("    total_liquidity:     {}", s.total_liquidity);
				println!("    reserved:            {}", s.reserved);
				println!("    available:           {}", s.available());
				println!("    active_invoices:     {}", s.invoices.len());
				println!("    active_offramps:     {}", s.offramps.len());
			},
			Err(e) => println!("    ERROR: failed to query pool: {}", e),
		},
		None => println!("    (Cardano not enabled)"),
	}
}
