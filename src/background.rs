use crate::cli;
use crate::mapping::{SwapDb, SwapStatus};
use crate::types::{ChannelManager, NetworkGraph, PeerManager};
use cardano_lightning_client::OperatorAgent;
use lightning::ln::msgs::SocketAddress;
use lightning::routing::gossip::NodeId;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Periodically reconnect to channel peers that have disconnected.
pub(crate) async fn reconnect_peers(
	channel_manager: Arc<ChannelManager>, peer_manager: Arc<PeerManager>,
	stop: Arc<AtomicBool>, network_graph: Arc<NetworkGraph>,
) {
	let mut interval = tokio::time::interval(Duration::from_secs(1));
	interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	loop {
		interval.tick().await;
		for node_id in channel_manager
			.list_channels()
			.iter()
			.map(|chan| chan.counterparty.node_id)
			.filter(|id| peer_manager.peer_by_node_id(id).is_none())
		{
			if stop.load(Ordering::Acquire) {
				return;
			}
			let id = NodeId::from_pubkey(&node_id);
			let addrs = if let Some(node) = network_graph.read_only().node(&id) {
				if let Some(ann) = &node.announcement_info {
					let non_onion = |addr| match addr {
						&lightning::ln::msgs::SocketAddress::OnionV2(_) => None,
						&lightning::ln::msgs::SocketAddress::OnionV3 { .. } => None,
						_ => Some(addr.clone()),
					};
					ann.addresses().iter().filter_map(non_onion).collect::<Vec<_>>()
				} else {
					Vec::new()
				}
			} else {
				Vec::new()
			};
			for addr in addrs {
				let sockaddrs = addr.to_socket_addrs();
				if sockaddrs.is_err() {
					continue;
				}
				for sockaddr in sockaddrs.unwrap() {
					let _ =
						cli::do_connect_peer(node_id, sockaddr, Arc::clone(&peer_manager)).await;
				}
			}
		}
	}
}

/// Periodically broadcast node_announcement for public channels.
pub(crate) async fn broadcast_node_announcement(
	peer_manager: Arc<PeerManager>, channel_manager: Arc<ChannelManager>,
	announced_node_name: [u8; 32], announced_listen_addr: Vec<SocketAddress>,
) {
	// First wait a minute until we have some peers and maybe have opened a channel.
	tokio::time::sleep(Duration::from_secs(60)).await;
	// Then, update our announcement once an hour to keep it fresh but avoid unnecessary churn
	// in the global gossip network.
	let mut interval = tokio::time::interval(Duration::from_secs(3600));
	loop {
		interval.tick().await;
		// Don't bother trying to announce if we don't have any public channls, though our
		// peers should drop such an announcement anyway. Note that announcement may not
		// propagate until we have a channel with 6+ confirmations.
		if channel_manager.list_channels().iter().any(|chan| chan.is_announced) {
			peer_manager.broadcast_node_announcement(
				[0; 3],
				announced_node_name,
				announced_listen_addr.clone(),
			);
		}
	}
}

/// Periodically check for expired swap mappings and cancel them on-chain.
pub(crate) async fn monitor_expired_swaps(
	operator: Arc<OperatorAgent>, swap_db: Arc<SwapDb>,
) {
	// Check every 60 seconds for expired swaps
	let mut interval = tokio::time::interval(Duration::from_secs(60));
	loop {
		interval.tick().await;

		let now_ms = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_millis() as i64;

		let expired = swap_db.get_expired_pending(now_ms);
		for mapping in &expired {
			println!("Cancelling expired swap invoice #{} (payment_hash: {})",
				mapping.invoice_id, mapping.payment_hash);

			let signed_tx = match operator.cancel_invoice(mapping.invoice_id).await {
				Ok(tx) => tx,
				Err(e) => {
					println!("ERROR: failed to build cancel tx for invoice #{}: {}",
						mapping.invoice_id, e);
					continue;
				},
			};

			match operator.submit_tx(&signed_tx).await {
				Ok(tx_hash) => {
					println!("SUCCESS: expired invoice #{} cancelled, tx: {}",
						mapping.invoice_id, tx_hash);
					swap_db.update_status(
						&mapping.payment_hash,
						SwapStatus::Expired,
						Some(&tx_hash),
					);
				},
				Err(e) => {
					println!("ERROR: failed to submit cancel tx for invoice #{}: {}",
						mapping.invoice_id, e);
				},
			}
		}
	}
}
