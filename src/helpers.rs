use cardano_lightning_client::OperatorAgent;
use std::time::Duration;

pub(crate) fn current_timestamp_ms() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_millis() as i64
}

pub(crate) async fn query_state_with_retry<T, F>(
	operator: &OperatorAgent,
	max_attempts: u32,
	delay: Duration,
	context: &str,
	find_fn: F,
) -> Result<T, String>
where
	F: Fn(&cardano_lightning_client::State) -> Option<T>,
{
	for attempt in 0..max_attempts {
		match operator.agent().query_state().await {
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
