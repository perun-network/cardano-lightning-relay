//! REST API for external clients to request swaps and query status.
//!
//! Endpoints:
//!   POST /swap/request  — Create a swap (LM invoice + BOLT11)
//!   GET  /swap/status/:hash — Query swap status
//!   GET  /pool/info     — Query pool state

use crate::cardano_swap;
use crate::cli::payment_cmds;
use crate::mapping::SwapDb;
use crate::types::{ChannelManager, InboundPaymentInfoStorage};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use cardano_lightning_client::OperatorAgent;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct ApiState {
	pub operator: Arc<OperatorAgent>,
	pub swap_db: Arc<SwapDb>,
	pub channel_manager: Arc<ChannelManager>,
	pub inbound_payments: Arc<Mutex<InboundPaymentInfoStorage>>,
}

#[derive(Deserialize)]
pub(crate) struct SwapRequest {
	pub amount_cbtc: i64,
	pub cardano_address: String,
}

#[derive(Serialize)]
pub(crate) struct SwapResponse {
	pub invoice_id: i64,
	pub bolt11: String,
	pub payment_hash: String,
}

#[derive(Serialize)]
pub(crate) struct SwapStatusResponse {
	pub payment_hash: String,
	pub invoice_id: i64,
	pub status: String,
	pub cardano_tx_hash: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PoolInfoResponse {
	pub total_liquidity: i64,
	pub reserved: i64,
	pub available: i64,
	pub active_invoices: usize,
}

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
	pub error: String,
}

pub(crate) fn create_router(state: ApiState) -> Router {
	Router::new()
		.route("/swap/request", post(handle_swap_request))
		.route("/swap/status/{hash}", get(handle_swap_status))
		.route("/pool/info", get(handle_pool_info))
		.with_state(state)
}

async fn handle_swap_request(
	State(state): State<ApiState>,
	Json(req): Json<SwapRequest>,
) -> Result<Json<SwapResponse>, Json<ErrorResponse>> {
	// 1. Create LM invoice on Cardano
	let (invoice_id, description) = cardano_swap::request_swap(
		&state.operator,
		&state.swap_db,
		req.amount_cbtc,
		&req.cardano_address,
	)
	.await
	.map_err(|e| Json(ErrorResponse { error: e }))?;

	// 2. Create BOLT11 invoice
	// Use 1 hour expiry (3600 seconds)
	let (bolt11, payment_hash) = {
		let mut inbound = state.inbound_payments.lock().unwrap();
		let result = payment_cmds::create_invoice_for_swap(
			req.amount_cbtc,
			&description,
			&mut inbound,
			&state.channel_manager,
			3600,
		);
		match result {
			Some((bolt11, hash)) => (bolt11, hash),
			None => {
				return Err(Json(ErrorResponse {
					error: "failed to create BOLT11 invoice".into(),
				}))
			},
		}
	};

	// 3. Store swap mapping
	let now_ms = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64;

	cardano_swap::store_swap_mapping(
		&state.swap_db,
		&payment_hash,
		invoice_id,
		req.amount_cbtc,
		&req.cardano_address,
		now_ms + 3_600_000,
	);

	Ok(Json(SwapResponse {
		invoice_id,
		bolt11,
		payment_hash,
	}))
}

async fn handle_swap_status(
	State(state): State<ApiState>,
	Path(hash): Path<String>,
) -> Result<Json<SwapStatusResponse>, Json<ErrorResponse>> {
	match state.swap_db.get_by_payment_hash(&hash) {
		Some(mapping) => Ok(Json(SwapStatusResponse {
			payment_hash: mapping.payment_hash,
			invoice_id: mapping.invoice_id,
			status: format!("{:?}", mapping.status),
			cardano_tx_hash: mapping.cardano_tx_hash,
		})),
		None => Err(Json(ErrorResponse {
			error: format!("swap not found for payment hash {}", hash),
		})),
	}
}

async fn handle_pool_info(
	State(state): State<ApiState>,
) -> Result<Json<PoolInfoResponse>, Json<ErrorResponse>> {
	match state.operator.agent().query_state().await {
		Ok(s) => Ok(Json(PoolInfoResponse {
			total_liquidity: s.total_liquidity,
			reserved: s.reserved,
			available: s.available(),
			active_invoices: s.invoices.len(),
		})),
		Err(e) => Err(Json(ErrorResponse {
			error: format!("failed to query pool: {}", e),
		})),
	}
}
