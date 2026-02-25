use crate::hex_utils;
use crate::types::{ChainMonitor, ChannelManager, NetworkGraph, PeerManager};
use bitcoin::secp256k1::PublicKey;
use lightning::chain::channelmonitor::Balance;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

pub(crate) async fn connect_peer_if_necessary(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManager>,
) -> Result<(), ()> {
	if peer_manager.peer_by_node_id(&pubkey).is_some() {
		return Ok(());
	}
	let res = do_connect_peer(pubkey, peer_addr, peer_manager).await;
	if res.is_err() {
		println!("ERROR: failed to connect to peer");
	}
	res
}

pub(crate) async fn do_connect_peer(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManager>,
) -> Result<(), ()> {
	match lightning_net_tokio::connect_outbound(Arc::clone(&peer_manager), pubkey, peer_addr).await
	{
		Some(connection_closed_future) => {
			let mut connection_closed_future = Box::pin(connection_closed_future);
			loop {
				tokio::select! {
					_ = &mut connection_closed_future => return Err(()),
					_ = tokio::time::sleep(Duration::from_millis(10)) => {},
				};
				if peer_manager.peer_by_node_id(&pubkey).is_some() {
					return Ok(());
				}
			}
		},
		None => Err(()),
	}
}

pub(crate) fn do_disconnect_peer(
	pubkey: PublicKey, peer_manager: Arc<PeerManager>,
	channel_manager: Arc<ChannelManager>,
) -> Result<(), ()> {
	//check for open channels with peer
	for channel in channel_manager.list_channels() {
		if channel.counterparty.node_id == pubkey {
			println!("Error: Node has an active channel with this peer, close any channels first");
			return Err(());
		}
	}

	//check the pubkey matches a valid connected peer
	if peer_manager.peer_by_node_id(&pubkey).is_none() {
		println!("Error: Could not find peer {}", pubkey);
		return Err(());
	}

	peer_manager.disconnect_by_node_id(pubkey);
	Ok(())
}

pub(crate) fn parse_peer_info(
	peer_pubkey_and_ip_addr: String,
) -> Result<(PublicKey, SocketAddr), std::io::Error> {
	let mut pubkey_and_addr = peer_pubkey_and_ip_addr.split("@");
	let pubkey = pubkey_and_addr.next();
	let peer_addr_str = pubkey_and_addr.next();
	if peer_addr_str.is_none() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::Other,
			"ERROR: incorrectly formatted peer info. Should be formatted as: `pubkey@host:port`",
		));
	}

	let peer_addr = peer_addr_str.unwrap().to_socket_addrs().map(|mut r| r.next());
	if peer_addr.is_err() || peer_addr.as_ref().unwrap().is_none() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::Other,
			"ERROR: couldn't parse pubkey@host:port into a socket address",
		));
	}

	let pubkey = hex_utils::to_compressed_pubkey(pubkey.unwrap());
	if pubkey.is_none() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::Other,
			"ERROR: unable to parse given pubkey for node",
		));
	}

	Ok((pubkey.unwrap(), peer_addr.unwrap().unwrap()))
}

pub(crate) fn list_peers(peer_manager: Arc<PeerManager>) {
	println!("\t{{");
	for peer_details in peer_manager.list_peers() {
		println!("\t\t pubkey: {}", peer_details.counterparty_node_id);
	}
	println!("\t}},");
}

pub(crate) fn node_info(
	channel_manager: &Arc<ChannelManager>, chain_monitor: &Arc<ChainMonitor>,
	peer_manager: &Arc<PeerManager>, network_graph: &Arc<NetworkGraph>,
) {
	println!("\t{{");
	println!("\t\t node_pubkey: {}", channel_manager.get_our_node_id());
	let chans = channel_manager.list_channels();
	println!("\t\t num_channels: {}", chans.len());
	println!("\t\t num_usable_channels: {}", chans.iter().filter(|c| c.is_usable).count());
	let balances = chain_monitor.get_claimable_balances(&[]);
	let local_balance_sat = balances.iter().map(|b| b.claimable_amount_satoshis()).sum::<u64>();
	println!("\t\t local_balance_sats: {}", local_balance_sat);
	let close_fees_map = |b| match b {
		&Balance::ClaimableOnChannelClose {
			ref balance_candidates,
			confirmed_balance_candidate_index,
			..
		} => balance_candidates[confirmed_balance_candidate_index].transaction_fee_satoshis,
		_ => 0,
	};
	let close_fees_sats = balances.iter().map(close_fees_map).sum::<u64>();
	println!("\t\t eventual_close_fees_sats: {}", close_fees_sats);
	let pending_payments_map = |b| match b {
		&Balance::MaybeTimeoutClaimableHTLC { amount_satoshis, outbound_payment, .. } => {
			if outbound_payment {
				amount_satoshis
			} else {
				0
			}
		},
		_ => 0,
	};
	let pending_payments = balances.iter().map(pending_payments_map).sum::<u64>();
	println!("\t\t pending_outbound_payments_sats: {}", pending_payments);
	println!("\t\t num_peers: {}", peer_manager.list_peers().len());
	let graph_lock = network_graph.read_only();
	println!("\t\t network_nodes: {}", graph_lock.nodes().len());
	println!("\t\t network_channels: {}", graph_lock.channels().len());
	println!("\t}},");
}
