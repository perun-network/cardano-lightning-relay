//! REST API for external clients to request swaps and query status.
//!
//! Endpoints:
//!   POST /swap/request       — Create an onramp swap (LM invoice + BOLT11)
//!   GET  /swap/status/:hash  — Query onramp swap status
//!   GET  /pool/info          — Query pool state
//!   POST /pool/deposit        — Deposit cBTC into pool
//!   POST /pool/withdraw       — Withdraw cBTC from pool
//!   POST /offramp/request    — Request offramp (cBTC → Lightning)
//!   GET  /offramp/status/:id — Query offramp status

use crate::cardano_offramp;
use crate::cardano_swap;
use crate::cli::payment_cmds;
use crate::helpers::current_timestamp_ms;
use crate::mapping::SwapDb;
use crate::types::{ChannelManager, InboundPaymentInfoStorage, OutboundPaymentInfoStorage};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use cardano_lightning_client::OperatorAgent;
use lightning_persister::fs_store::FilesystemStore;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(crate) struct ApiState {
	pub operator: Arc<OperatorAgent>,
	pub swap_db: Arc<SwapDb>,
	pub channel_manager: Arc<ChannelManager>,
	pub inbound_payments: Arc<Mutex<InboundPaymentInfoStorage>>,
	pub outbound_payments: Arc<Mutex<OutboundPaymentInfoStorage>>,
	pub fs_store: Arc<FilesystemStore>,
}

// ─── Onramp types ────────────────────────────────────────────────────────────

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
	pub create_tx_hash: Option<String>,
}

// ─── Offramp types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct OfframpRequest {
	pub bolt11: String,
	pub amount_cbtc: i64,
	pub cardano_address: String,
}

#[derive(Deserialize)]
pub(crate) struct OfframpDepositRequest {
	pub offramp_id: i64,
	pub cbtc_tx_hash: String,
}

#[derive(Serialize)]
pub(crate) struct OfframpResponse {
	pub offramp_id: i64,
	pub operator_address: String,
	pub payment_hash: String,
	pub status: String,
}

#[derive(Serialize)]
pub(crate) struct OfframpStatusResponse {
	pub offramp_id: i64,
	pub payment_hash: String,
	pub amount_cbtc: i64,
	pub status: String,
	pub deposit_tx_hash: Option<String>,
	pub create_offramp_tx_hash: Option<String>,
	pub lightning_preimage: Option<String>,
	pub error_message: Option<String>,
}

// ─── Common types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct PoolInfoResponse {
	pub total_liquidity: i64,
	pub reserved: i64,
	pub available: i64,
	pub active_invoices: usize,
}

// ─── Pool deposit types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PoolDepositRequest {
	pub amount: i64,
}

#[derive(Serialize)]
pub(crate) struct PoolDepositResponse {
	pub tx_hash: String,
	pub amount: i64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub new_total_liquidity: Option<i64>,
}

// ─── Pool withdraw types ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct PoolWithdrawRequest {
	pub amount: i64,
}

#[derive(Serialize)]
pub(crate) struct PoolWithdrawResponse {
	pub tx_hash: String,
	pub amount: i64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub new_total_liquidity: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
	pub error: String,
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub(crate) fn create_router(state: ApiState) -> Router {
	Router::new()
		.route("/swap/request", post(handle_swap_request))
		.route("/swap/status/{hash}", get(handle_swap_status))
		.route("/pool/info", get(handle_pool_info))
		.route("/pool/deposit", post(handle_pool_deposit))
		.route("/pool/withdraw", post(handle_pool_withdraw))
		.route("/offramp/request", post(handle_offramp_request))
		.route("/offramp/deposit", post(handle_offramp_deposit))
		.route("/offramp/status/{id}", get(handle_offramp_status))
		.with_state(state)
}

// ─── Onramp handlers ────────────────────────────────────────────────────────

async fn handle_swap_request(
	State(state): State<ApiState>,
	Json(req): Json<SwapRequest>,
) -> Result<Json<SwapResponse>, Json<ErrorResponse>> {
	// 1. Create LM invoice on Cardano
	let (invoice_id, description, create_tx_hash) = cardano_swap::request_swap(
		&*state.operator,
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
	cardano_swap::store_swap_mapping(
		&state.swap_db,
		&payment_hash,
		invoice_id,
		req.amount_cbtc,
		&req.cardano_address,
		current_timestamp_ms() + 3_600_000,
		&create_tx_hash,
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
			create_tx_hash: mapping.create_tx_hash,
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

async fn handle_pool_deposit(
	State(state): State<ApiState>,
	Json(req): Json<PoolDepositRequest>,
) -> Result<Json<PoolDepositResponse>, Json<ErrorResponse>> {
	let signed_tx = state
		.operator
		.deposit(req.amount)
		.await
		.map_err(|e| Json(ErrorResponse { error: format!("deposit failed: {}", e) }))?;

	let tx_hash = state
		.operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| Json(ErrorResponse { error: format!("submit failed: {}", e) }))?;

	let new_total = state.operator.agent().query_state().await
		.ok()
		.map(|s| s.total_liquidity);

	Ok(Json(PoolDepositResponse {
		tx_hash,
		amount: req.amount,
		new_total_liquidity: new_total,
	}))
}

async fn handle_pool_withdraw(
	State(state): State<ApiState>,
	Json(req): Json<PoolWithdrawRequest>,
) -> Result<Json<PoolWithdrawResponse>, Json<ErrorResponse>> {
	let signed_tx = state
		.operator
		.withdraw(req.amount)
		.await
		.map_err(|e| Json(ErrorResponse { error: format!("withdraw failed: {}", e) }))?;

	let tx_hash = state
		.operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| Json(ErrorResponse { error: format!("submit failed: {}", e) }))?;

	let new_total = state.operator.agent().query_state().await
		.ok()
		.map(|s| s.total_liquidity);

	Ok(Json(PoolWithdrawResponse {
		tx_hash,
		amount: req.amount,
		new_total_liquidity: new_total,
	}))
}

// ─── Offramp handlers ───────────────────────────────────────────────────────

async fn handle_offramp_request(
	State(state): State<ApiState>,
	Json(req): Json<OfframpRequest>,
) -> Result<Json<OfframpResponse>, Json<ErrorResponse>> {
	let (offramp_id, operator_address, payment_hash) = cardano_offramp::request_offramp(
		&*state.operator,
		&state.swap_db,
		&req.bolt11,
		req.amount_cbtc,
		&req.cardano_address,
	)
	.await
	.map_err(|e| Json(ErrorResponse { error: e }))?;

	Ok(Json(OfframpResponse {
		offramp_id,
		operator_address,
		payment_hash,
		status: "AwaitingDeposit".to_string(),
	}))
}

async fn handle_offramp_deposit(
	State(state): State<ApiState>,
	Json(req): Json<OfframpDepositRequest>,
) -> Result<Json<OfframpResponse>, Json<ErrorResponse>> {
	cardano_offramp::process_offramp_deposit(
		&state.operator,
		&state.swap_db,
		&state.channel_manager,
		&state.outbound_payments,
		&state.fs_store,
		req.offramp_id,
		&req.cbtc_tx_hash,
	)
	.await
	.map_err(|e| Json(ErrorResponse { error: e }))?;

	// Get the updated mapping for response
	let mapping = state.swap_db.get_offramp_by_id(req.offramp_id)
		.ok_or_else(|| Json(ErrorResponse { error: "offramp not found".into() }))?;

	Ok(Json(OfframpResponse {
		offramp_id: req.offramp_id,
		operator_address: state.operator.config().operator_address.clone(),
		payment_hash: mapping.payment_hash,
		status: mapping.status.as_str().to_string(),
	}))
}

async fn handle_offramp_status(
	State(state): State<ApiState>,
	Path(id): Path<i64>,
) -> Result<Json<OfframpStatusResponse>, Json<ErrorResponse>> {
	match state.swap_db.get_offramp_by_id(id) {
		Some(mapping) => Ok(Json(OfframpStatusResponse {
			offramp_id: mapping.offramp_id,
			payment_hash: mapping.payment_hash,
			amount_cbtc: mapping.amount_cbtc,
			status: mapping.status.as_str().to_string(),
			deposit_tx_hash: mapping.deposit_tx_hash,
			create_offramp_tx_hash: mapping.cardano_offramp_tx_hash,
			lightning_preimage: mapping.lightning_preimage,
			error_message: mapping.error_message,
		})),
		None => Err(Json(ErrorResponse {
			error: format!("offramp not found for id {}", id),
		})),
	}
}
