pub(crate) const MSATS_PER_CBTC_BASE_UNIT: u64 = 100_000;

pub(crate) fn cbtc_to_msat(amount_cbtc: i64) -> Option<u64> {
	if amount_cbtc <= 0 {
		return None;
	}

	(amount_cbtc as u64).checked_mul(MSATS_PER_CBTC_BASE_UNIT)
}

#[cfg(test)]
mod tests {
	use super::cbtc_to_msat;

	#[test]
	fn converts_cbtc_base_units_to_lightning_msats() {
		assert_eq!(cbtc_to_msat(1), Some(100_000));
		assert_eq!(cbtc_to_msat(1_000_000), Some(100_000_000_000));
	}

	#[test]
	fn rejects_non_positive_cbtc_amounts() {
		assert_eq!(cbtc_to_msat(0), None);
		assert_eq!(cbtc_to_msat(-1), None);
	}
}
