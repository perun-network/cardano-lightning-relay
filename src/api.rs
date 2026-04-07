//! REST API for external clients to request swaps and query status.
//!
//! Public endpoints (no auth):
//!   POST /swap/request       — Create an onramp swap (LM invoice + BOLT11)
//!   GET  /swap/status/:hash  — Query onramp swap status
//!   GET  /pool/info          — Query pool state
//!   POST /offramp/request    — Request offramp (cBTC → Lightning)
//!   POST /offramp/deposit    — Notify relay of cBTC deposit
//!   GET  /offramp/status/:id — Query offramp status
//!
//! Operator endpoints (bearer token required):
//!   POST /pool/deposit       — Deposit cBTC into pool
//!   POST /pool/withdraw      — Withdraw cBTC from pool

use crate::cardano_offramp;
use crate::cardano_swap;
use crate::cli::payment_cmds;
use crate::helpers::current_timestamp_ms;
use crate::mapping::SwapDb;
use crate::types::{ChannelManager, InboundPaymentInfoStorage, OutboundPaymentInfoStorage};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use cardano_lightning_client::OperatorAgent;
use lightning_persister::fs_store::FilesystemStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub(crate) struct ApiState {
	pub operator: Arc<OperatorAgent>,
	pub swap_db: Arc<SwapDb>,
	pub channel_manager: Arc<ChannelManager>,
	pub inbound_payments: Arc<Mutex<InboundPaymentInfoStorage>>,
	pub outbound_payments: Arc<Mutex<OutboundPaymentInfoStorage>>,
	pub fs_store: Arc<FilesystemStore>,
	pub auth_token: Option<String>,
	pub rate_limiter: Arc<Mutex<RateLimiter>>,
	pub max_active_swaps: i64,
	pub max_active_offramps: i64,
	/// Swap/offramp expiry in milliseconds (default: 3_600_000 = 1 hour).
	pub swap_expiry_ms: i64,
}

/// Simple in-memory rate limiter: max requests per IP per window.
pub(crate) struct RateLimiter {
	requests: HashMap<std::net::IpAddr, (u32, std::time::Instant)>,
	max_per_window: u32,
	window: std::time::Duration,
}

impl RateLimiter {
	pub fn new(max_per_window: u32, window_secs: u64) -> Self {
		Self {
			requests: HashMap::new(),
			max_per_window,
			window: std::time::Duration::from_secs(window_secs),
		}
	}

	pub fn check(&mut self, ip: std::net::IpAddr) -> bool {
		let now = std::time::Instant::now();
		let entry = self.requests.entry(ip).or_insert((0, now));
		if now.duration_since(entry.1) > self.window {
			*entry = (1, now);
			true
		} else if entry.0 < self.max_per_window {
			entry.0 += 1;
			true
		} else {
			false
		}
	}
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
	/// Active swaps tracked by the relay DB (may be ahead of on-chain state).
	pub pending_swaps: i64,
}

#[derive(Serialize)]
pub(crate) struct RelayInfoResponse {
	pub operator_address: String,
	pub script_address: String,
	pub cbtc_policy_id: String,
	pub cbtc_asset_name: String,
	pub exchange_rate: f64,
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

// ─── History / Metrics types ────────────────────────────────────────────────

#[derive(Serialize)]
pub(crate) struct MetricsResponse {
	pub onramp: StatusCounts,
	pub offramp: StatusCounts,
}

#[derive(Serialize)]
pub(crate) struct StatusCounts {
	pub completed: i64,
	pub pending: i64,
	pub failed: i64,
	pub total: i64,
}

#[derive(Serialize)]
pub(crate) struct ErrorResponse {
	pub error: String,
}

// ─── Auth helper ─────────────────────────────────────────────────────────────

fn verify_operator_auth(
	headers: &HeaderMap, auth_token: &Option<String>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
	let token = match auth_token {
		Some(t) => t,
		None => return Ok(()), // no token configured = auth disabled
	};
	let header = headers
		.get("authorization")
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "));
	match header {
		Some(provided) if provided == token => Ok(()),
		_ => Err((
			StatusCode::UNAUTHORIZED,
			Json(ErrorResponse { error: "unauthorized: invalid or missing bearer token".into() }),
		)),
	}
}

// ─── Router ──────────────────────────────────────────────────────────────────

pub(crate) fn create_router(state: ApiState) -> Router {
	Router::new()
		// Public endpoints
		.route("/info", get(handle_relay_info))
		.route("/swap/request", post(handle_swap_request))
		.route("/swap/status/{hash}", get(handle_swap_status))
		.route("/swap/history", get(handle_swap_history))
		.route("/pool/info", get(handle_pool_info))
		.route("/offramp/request", post(handle_offramp_request))
		.route("/offramp/deposit", post(handle_offramp_deposit))
		.route("/offramp/status/{id}", get(handle_offramp_status))
		.route("/offramp/history", get(handle_offramp_history))
		.route("/metrics", get(handle_metrics))
		// Operator endpoints (bearer token required)
		.route("/pool/deposit", post(handle_pool_deposit))
		.route("/pool/withdraw", post(handle_pool_withdraw))
		.with_state(state)
		.layer(CorsLayer::permissive())
}

// ─── Onramp handlers ────────────────────────────────────────────────────────

async fn handle_swap_request(
	State(state): State<ApiState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(req): Json<SwapRequest>,
) -> Result<Json<SwapResponse>, (StatusCode, Json<ErrorResponse>)> {
	if !state.rate_limiter.lock().unwrap().check(addr.ip()) {
		return Err((StatusCode::TOO_MANY_REQUESTS,
			Json(ErrorResponse { error: "rate limit exceeded, try again later".into() })));
	}

	// Input validation
	if req.amount_cbtc <= 0 || req.amount_cbtc > 10_000_000_000 {
		return Err((StatusCode::BAD_REQUEST,
			Json(ErrorResponse { error: "amount must be between 1 and 10,000,000,000 cBTC".into() })));
	}

	// Reject if too many active swaps (prevents pool liquidity lockup via spam)
	let active_swaps = state.swap_db.count_active_swaps();
	if active_swaps >= state.max_active_swaps {
		return Err((StatusCode::SERVICE_UNAVAILABLE,
			Json(ErrorResponse { error: format!(
				"too many active swaps ({}), try again later", active_swaps,
			)})));
	}

	// Liquidity pre-check
	if let Ok(pool_state) = state.operator.agent().query_state().await {
		if req.amount_cbtc > pool_state.available() {
			return Err((StatusCode::BAD_REQUEST,
				Json(ErrorResponse { error: format!(
					"insufficient pool liquidity: requested {} but only {} available",
					req.amount_cbtc, pool_state.available()
				)})));
		}
	}

	let expiry_ms = state.swap_expiry_ms;
	let expiry_secs = (expiry_ms / 1000) as u32;

	// 1. Create LM invoice on Cardano
	let (invoice_id, description, create_tx_hash) = cardano_swap::request_swap(
		&*state.operator,
		req.amount_cbtc,
		&req.cardano_address,
		expiry_ms,
	)
	.await
	.map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })))?;

	// 2. Create BOLT11 invoice
	let (bolt11, payment_hash) = {
		let mut inbound = state.inbound_payments.lock().unwrap();
		let result = payment_cmds::create_invoice_for_swap(
			req.amount_cbtc,
			&description,
			&mut inbound,
			&state.channel_manager,
			expiry_secs,
		);
		match result {
			Some((bolt11, hash)) => (bolt11, hash),
			None => {
				return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
					error: "failed to create BOLT11 invoice".into(),
				})))
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
		current_timestamp_ms() + expiry_ms,
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

async fn handle_relay_info(
	State(state): State<ApiState>,
) -> Json<RelayInfoResponse> {
	let config = state.operator.config();
	Json(RelayInfoResponse {
		operator_address: config.operator_address.clone(),
		script_address: state.operator.agent().config().script_address.clone(),
		cbtc_policy_id: config.cbtc_policy.clone(),
		cbtc_asset_name: config.cbtc_name.clone(),
		exchange_rate: 1.0, // TODO: configurable exchange rate
	})
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
			pending_swaps: state.swap_db.count_active_swaps(),
		})),
		Err(e) => Err(Json(ErrorResponse {
			error: format!("failed to query pool: {}", e),
		})),
	}
}

async fn handle_pool_deposit(
	State(state): State<ApiState>,
	headers: HeaderMap,
	Json(req): Json<PoolDepositRequest>,
) -> Result<Json<PoolDepositResponse>, (StatusCode, Json<ErrorResponse>)> {
	verify_operator_auth(&headers, &state.auth_token)?;
	let signed_tx = state
		.operator
		.deposit(req.amount)
		.await
		.map_err(|e| (StatusCode::BAD_REQUEST,
			Json(ErrorResponse { error: format!("deposit failed: {}", e) })))?;

	let tx_hash = state
		.operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
			Json(ErrorResponse { error: format!("submit failed: {}", e) })))?;

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
	headers: HeaderMap,
	Json(req): Json<PoolWithdrawRequest>,
) -> Result<Json<PoolWithdrawResponse>, (StatusCode, Json<ErrorResponse>)> {
	verify_operator_auth(&headers, &state.auth_token)?;
	let signed_tx = state
		.operator
		.withdraw(req.amount)
		.await
		.map_err(|e| (StatusCode::BAD_REQUEST,
			Json(ErrorResponse { error: format!("withdraw failed: {}", e) })))?;

	let tx_hash = state
		.operator
		.submit_tx(&signed_tx)
		.await
		.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,
			Json(ErrorResponse { error: format!("submit failed: {}", e) })))?;

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
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(req): Json<OfframpRequest>,
) -> Result<Json<OfframpResponse>, (StatusCode, Json<ErrorResponse>)> {
	if !state.rate_limiter.lock().unwrap().check(addr.ip()) {
		return Err((StatusCode::TOO_MANY_REQUESTS,
			Json(ErrorResponse { error: "rate limit exceeded, try again later".into() })));
	}

	// Input validation
	if req.amount_cbtc <= 0 || req.amount_cbtc > 10_000_000_000 {
		return Err((StatusCode::BAD_REQUEST,
			Json(ErrorResponse { error: "amount must be between 1 and 10,000,000,000 cBTC".into() })));
	}

	// Reject if too many active offramps (prevents operator ADA drain via spam)
	let active_offramps = state.swap_db.count_active_offramps();
	if active_offramps >= state.max_active_offramps {
		return Err((StatusCode::SERVICE_UNAVAILABLE,
			Json(ErrorResponse { error: format!(
				"too many active offramps ({}), try again later", active_offramps,
			)})));
	}

	let (offramp_id, operator_address, payment_hash) = cardano_offramp::request_offramp(
		&*state.operator,
		&state.swap_db,
		&req.bolt11,
		req.amount_cbtc,
		&req.cardano_address,
		state.swap_expiry_ms,
	)
	.await
	.map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })))?;

	Ok(Json(OfframpResponse {
		offramp_id,
		operator_address,
		payment_hash,
		status: "AwaitingDeposit".to_string(),
	}))
}

async fn handle_offramp_deposit(
	State(state): State<ApiState>,
	ConnectInfo(addr): ConnectInfo<SocketAddr>,
	Json(req): Json<OfframpDepositRequest>,
) -> Result<Json<OfframpResponse>, (StatusCode, Json<ErrorResponse>)> {
	if !state.rate_limiter.lock().unwrap().check(addr.ip()) {
		return Err((StatusCode::TOO_MANY_REQUESTS,
			Json(ErrorResponse { error: "rate limit exceeded, try again later".into() })));
	}
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
	.map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })))?;

	// Get the updated mapping for response
	let mapping = state.swap_db.get_offramp_by_id(req.offramp_id)
		.ok_or_else(|| (StatusCode::NOT_FOUND,
			Json(ErrorResponse { error: "offramp not found".into() })))?;

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

// ─── History / Metrics handlers ─────────────────────────────────────────────

async fn handle_swap_history(
	State(state): State<ApiState>,
) -> Json<Vec<SwapStatusResponse>> {
	let swaps = state.swap_db.list_recent_swaps(20);
	Json(swaps.into_iter().map(|m| SwapStatusResponse {
		payment_hash: m.payment_hash,
		invoice_id: m.invoice_id,
		status: format!("{:?}", m.status),
		cardano_tx_hash: m.cardano_tx_hash,
		create_tx_hash: m.create_tx_hash,
	}).collect())
}

async fn handle_offramp_history(
	State(state): State<ApiState>,
) -> Json<Vec<OfframpStatusResponse>> {
	let offramps = state.swap_db.list_recent_offramps(20);
	Json(offramps.into_iter().map(|m| OfframpStatusResponse {
		offramp_id: m.offramp_id,
		payment_hash: m.payment_hash,
		amount_cbtc: m.amount_cbtc,
		status: m.status.as_str().to_string(),
		deposit_tx_hash: m.deposit_tx_hash,
		create_offramp_tx_hash: m.cardano_offramp_tx_hash,
		lightning_preimage: m.lightning_preimage,
		error_message: m.error_message,
	}).collect())
}

async fn handle_metrics(
	State(state): State<ApiState>,
) -> Json<MetricsResponse> {
	let swap_counts = state.swap_db.get_swap_counts();
	let offramp_counts = state.swap_db.get_offramp_counts();

	Json(MetricsResponse {
		onramp: to_status_counts(&swap_counts),
		offramp: to_status_counts(&offramp_counts),
	})
}

fn to_status_counts(counts: &[(String, i64)]) -> StatusCounts {
	let mut completed = 0;
	let mut pending = 0;
	let mut failed = 0;
	let mut total = 0;
	for (status, count) in counts {
		total += count;
		match status.as_str() {
			"completed" => completed += count,
			"failed" | "expired" => failed += count,
			_ => pending += count,
		}
	}
	StatusCounts { completed, pending, failed, total }
}

#[cfg(test)]
mod tests {
	use super::*;
	use axum::http::HeaderValue;

	// ─── RateLimiter tests ──────────────────────────────────────────────────

	#[test]
	fn rate_limiter_allows_up_to_max() {
		let mut limiter = RateLimiter::new(3, 60);
		let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();

		assert!(limiter.check(ip));
		assert!(limiter.check(ip));
		assert!(limiter.check(ip));
		assert!(!limiter.check(ip)); // 4th request blocked
	}

	#[test]
	fn rate_limiter_isolates_ips() {
		let mut limiter = RateLimiter::new(1, 60);
		let ip1: std::net::IpAddr = "10.0.0.1".parse().unwrap();
		let ip2: std::net::IpAddr = "10.0.0.2".parse().unwrap();

		assert!(limiter.check(ip1));
		assert!(!limiter.check(ip1)); // ip1 blocked
		assert!(limiter.check(ip2));  // ip2 still allowed
	}

	#[test]
	fn rate_limiter_resets_after_window() {
		let mut limiter = RateLimiter::new(1, 0); // 0-second window
		let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();

		assert!(limiter.check(ip));
		// Window is 0 seconds, so next check should see expired window
		std::thread::sleep(std::time::Duration::from_millis(5));
		assert!(limiter.check(ip)); // allowed again after window expires
	}

	// ─── Auth tests ─────────────────────────────────────────────────────────

	fn make_headers(auth: Option<&str>) -> HeaderMap {
		let mut headers = HeaderMap::new();
		if let Some(val) = auth {
			headers.insert("authorization", HeaderValue::from_str(val).unwrap());
		}
		headers
	}

	#[test]
	fn auth_disabled_when_no_token_configured() {
		let headers = make_headers(None);
		assert!(verify_operator_auth(&headers, &None).is_ok());
	}

	#[test]
	fn auth_succeeds_with_correct_token() {
		let token = Some("secret123".to_string());
		let headers = make_headers(Some("Bearer secret123"));
		assert!(verify_operator_auth(&headers, &token).is_ok());
	}

	#[test]
	fn auth_fails_with_wrong_token() {
		let token = Some("secret123".to_string());
		let headers = make_headers(Some("Bearer wrong_token"));
		let result = verify_operator_auth(&headers, &token);
		assert!(result.is_err());
		let (status, _) = result.unwrap_err();
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}

	#[test]
	fn auth_fails_with_missing_header() {
		let token = Some("secret123".to_string());
		let headers = make_headers(None);
		let result = verify_operator_auth(&headers, &token);
		assert!(result.is_err());
		let (status, _) = result.unwrap_err();
		assert_eq!(status, StatusCode::UNAUTHORIZED);
	}

	#[test]
	fn auth_fails_with_non_bearer_scheme() {
		let token = Some("secret123".to_string());
		let headers = make_headers(Some("Basic secret123"));
		let result = verify_operator_auth(&headers, &token);
		assert!(result.is_err());
	}

	// ─── to_status_counts tests ─────────────────────────────────────────────

	#[test]
	fn status_counts_aggregation() {
		let counts = vec![
			("completed".to_string(), 10),
			("pending".to_string(), 3),
			("fulfilling".to_string(), 2),
			("failed".to_string(), 5),
			("expired".to_string(), 1),
		];
		let result = to_status_counts(&counts);
		assert_eq!(result.completed, 10);
		assert_eq!(result.pending, 5);  // pending + fulfilling
		assert_eq!(result.failed, 6);   // failed + expired
		assert_eq!(result.total, 21);
	}

	#[test]
	fn status_counts_empty() {
		let result = to_status_counts(&[]);
		assert_eq!(result.completed, 0);
		assert_eq!(result.pending, 0);
		assert_eq!(result.failed, 0);
		assert_eq!(result.total, 0);
	}

	#[test]
	fn status_counts_unknown_status_counted_as_pending() {
		let counts = vec![
			("some_new_status".to_string(), 7),
		];
		let result = to_status_counts(&counts);
		assert_eq!(result.pending, 7);
		assert_eq!(result.total, 7);
	}

}
