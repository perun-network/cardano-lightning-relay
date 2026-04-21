use crate::cardano_ops::CardanoOperator;
use cardano_lightning_client::CardanoError;
use std::future::Future;
use std::time::Duration;

/// Returns true if the error string represents a transient submission failure
/// that should be retried — typically caused by Blockfrost indexing lag where
/// the relay built a TX referencing a UTxO that was just consumed.
pub(crate) fn is_retryable_submit_err(err_str: &str) -> bool {
	err_str.contains("BadInputs")
		|| err_str.contains("already been included")
		|| err_str.contains("ConwayMempoolFailure")
}

/// Build + submit a contract TX with retry on transient Blockfrost-lag errors.
///
/// `build_and_submit` is invoked up to `max_attempts` times. Between attempts we
/// sleep `delay` (gives Blockfrost time to index the previous on-chain TX so the
/// next build sees fresh UTxO state).
///
/// Returns Ok on first success. Returns Err immediately on a non-retryable error.
/// Returns Err after exhausting attempts on a retryable error.
pub(crate) async fn submit_contract_tx_with_retry<F, Fut, T>(
	context: &str, max_attempts: u32, delay: Duration, build_and_submit: F,
) -> Result<T, String>
where
	F: Fn() -> Fut,
	Fut: Future<Output = Result<T, CardanoError>>,
{
	let mut last_err = String::new();
	for attempt in 0..max_attempts {
		if attempt > 0 {
			println!(
				"{}: retry {}/{} (waiting {}s for Blockfrost indexing)...",
				context, attempt + 1, max_attempts, delay.as_secs(),
			);
			tokio::time::sleep(delay).await;
		}
		match build_and_submit().await {
			Ok(result) => return Ok(result),
			Err(e) => {
				let err_str = format!("{}", e);
				last_err = err_str.clone();
				if !is_retryable_submit_err(&err_str) {
					return Err(format!("{}: non-retryable error: {}", context, err_str));
				}
			},
		}
	}
	Err(format!("{}: failed after {} attempts: {}", context, max_attempts, last_err))
}

pub(crate) fn current_timestamp_ms() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64
}

pub(crate) async fn query_state_with_retry<T, F>(
	operator: &impl CardanoOperator,
	max_attempts: u32,
	delay: Duration,
	context: &str,
	find_fn: F,
) -> Result<T, String>
where
	F: Fn(&cardano_lightning_client::State) -> Option<T>,
{
	for attempt in 0..max_attempts {
		match operator.query_state().await {
			Ok(state) => {
				if let Some(item) = find_fn(&state) {
					return Ok(item);
				}
				if attempt < max_attempts - 1 {
					println!(
						"{}: not yet in on-chain state (attempt {}/{}), waiting...",
						context, attempt + 1, max_attempts,
					);
					tokio::time::sleep(delay).await;
				}
			},
			Err(e) => {
				if attempt < max_attempts - 1 {
					println!("{}: state query failed ({}), retrying...", context, e);
					tokio::time::sleep(delay).await;
				} else {
					return Err(format!("state query failed after {} attempts: {}", max_attempts, e));
				}
			},
		}
	}
	Err(format!("{}: not found in on-chain state after {} attempts", context, max_attempts))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn timestamp_is_reasonable() {
		let ts = current_timestamp_ms();
		// Must be after Feb 2024 (1_706_745_600_000 ms)
		assert!(ts > 1_706_745_600_000, "timestamp {} is before Feb 2024", ts);
		// Must be before year 2100 (4_102_444_800_000 ms)
		assert!(ts < 4_102_444_800_000, "timestamp {} is absurdly far in the future", ts);
	}
}
