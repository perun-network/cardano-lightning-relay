use crate::disk::OUTBOUND_PAYMENTS_FNAME;
use crate::types::{
	ChannelManager, HTLCStatus, InboundPaymentInfoStorage, MillisatAmount,
	OutboundPaymentInfoStorage, PaymentInfo,
};
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::PublicKey;
use lightning::ln::channelmanager::{Bolt11InvoiceParameters, PaymentId, RecipientOnionFields, Retry};
use lightning_invoice::{Bolt11InvoiceDescription, Description};
use lightning::routing::router::{PaymentParameters, RouteParameters, RouteParametersConfig};
use lightning::sign::EntropySource;
use lightning::types::payment::{PaymentHash, PaymentPreimage};
use lightning::util::persist::KVStore;
use lightning::util::ser::Writeable;
use lightning_persister::fs_store::FilesystemStore;
use std::sync::Mutex;
use std::time::Duration;

pub(crate) async fn send_payment(
	channel_manager: &ChannelManager, invoice: &lightning_invoice::Bolt11Invoice,
	required_amount_msat: Option<u64>, outbound_payments: &Mutex<OutboundPaymentInfoStorage>,
	fs_store: &FilesystemStore,
) {
	let payment_id = PaymentId((*invoice.payment_hash()).to_byte_array());
	let payment_secret = Some(*invoice.payment_secret());
	let amt_msat = match (invoice.amount_milli_satoshis(), required_amount_msat) {
		// pay_for_bolt11_invoice only validates that the amount we pay is >= the invoice's
		// required amount, not that its equal (to allow for overpayment). As that is somewhat
		// surprising, here we check and reject all disagreements in amount.
		(Some(inv_amt), Some(req_amt)) if inv_amt != req_amt => {
			println!(
				"Amount didn't match invoice value of {}msat",
				invoice.amount_milli_satoshis().unwrap_or(0)
			);
			print!("> ");
			return;
		},
		(Some(inv_amt), _) => inv_amt,
		(_, Some(req_amt)) => req_amt,
		(None, None) => {
			println!("Need an amount to pay an amountless invoice");
			print!("> ");
			return;
		},
	};
	let write_future = {
		let mut outbound_payments = outbound_payments.lock().unwrap();
		outbound_payments.payments.insert(
			payment_id,
			PaymentInfo {
				preimage: None,
				secret: payment_secret,
				status: HTLCStatus::Pending,
				amt_msat: MillisatAmount(Some(amt_msat)),
			},
		);
		fs_store.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
	};
	write_future.await.unwrap();

	match channel_manager.pay_for_bolt11_invoice(
		invoice,
		payment_id,
		required_amount_msat,
		RouteParametersConfig::default(),
		Retry::Timeout(Duration::from_secs(10)),
	) {
		Ok(_) => {
			let payee_pubkey = invoice.recover_payee_pub_key();
			println!("EVENT: initiated sending {} msats to {}", amt_msat, payee_pubkey);
			print!("> ");
		},
		Err(e) => {
			println!("ERROR: failed to send payment: {:?}", e);
			print!("> ");
			let write_future = {
				let mut outbound_payments = outbound_payments.lock().unwrap();
				outbound_payments.payments.get_mut(&payment_id).unwrap().status =
					HTLCStatus::Failed;
				fs_store.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
			};
			write_future.await.unwrap();
		},
	};
}

pub(crate) async fn keysend<E: EntropySource>(
	channel_manager: &ChannelManager, payee_pubkey: PublicKey, amt_msat: u64, entropy_source: &E,
	outbound_payments: &Mutex<OutboundPaymentInfoStorage>, fs_store: &FilesystemStore,
) {
	let payment_preimage = PaymentPreimage(entropy_source.get_secure_random_bytes());
	let payment_id = PaymentId(Sha256::hash(&payment_preimage.0[..]).to_byte_array());

	let route_params = RouteParameters::from_payment_params_and_value(
		PaymentParameters::for_keysend(payee_pubkey, 40, false),
		amt_msat,
	);
	let write_future = {
		let mut outbound_payments = outbound_payments.lock().unwrap();
		outbound_payments.payments.insert(
			payment_id,
			PaymentInfo {
				preimage: None,
				secret: None,
				status: HTLCStatus::Pending,
				amt_msat: MillisatAmount(Some(amt_msat)),
			},
		);
		fs_store.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
	};
	write_future.await.unwrap();
	match channel_manager.send_spontaneous_payment(
		Some(payment_preimage),
		RecipientOnionFields::spontaneous_empty(),
		payment_id,
		route_params,
		Retry::Timeout(Duration::from_secs(10)),
	) {
		Ok(_payment_hash) => {
			println!("EVENT: initiated sending {} msats to {}", amt_msat, payee_pubkey);
			print!("> ");
		},
		Err(e) => {
			println!("ERROR: failed to send payment: {:?}", e);
			print!("> ");
			let write_future = {
				let mut outbound_payments = outbound_payments.lock().unwrap();
				outbound_payments.payments.get_mut(&payment_id).unwrap().status =
					HTLCStatus::Failed;
				fs_store.write("", "", OUTBOUND_PAYMENTS_FNAME, outbound_payments.encode())
			};
			write_future.await.unwrap();
		},
	};
}

pub(crate) fn get_invoice(
	amt_msat: u64, inbound_payments: &mut InboundPaymentInfoStorage,
	channel_manager: &ChannelManager, expiry_secs: u32,
) {
	let mut invoice_params: Bolt11InvoiceParameters = Default::default();
	invoice_params.amount_msats = Some(amt_msat);
	invoice_params.invoice_expiry_delta_secs = Some(expiry_secs);
	let invoice = match channel_manager.create_bolt11_invoice(invoice_params) {
		Ok(inv) => {
			println!("SUCCESS: generated invoice: {}", inv);
			inv
		},
		Err(e) => {
			println!("ERROR: failed to create invoice: {:?}", e);
			return;
		},
	};

	let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
	inbound_payments.payments.insert(
		payment_hash,
		PaymentInfo {
			preimage: None,
			secret: Some(invoice.payment_secret().clone()),
			status: HTLCStatus::Pending,
			amt_msat: MillisatAmount(Some(amt_msat)),
		},
	);
}

/// Create a BOLT11 invoice for a swap request.
/// Returns (bolt11_string, payment_hash_hex) on success.
pub(crate) fn create_invoice_for_swap(
	amount_cbtc: i64, description: &str, inbound_payments: &mut InboundPaymentInfoStorage,
	channel_manager: &ChannelManager, expiry_secs: u32,
) -> Option<(String, String)> {
	// Convert cBTC amount to msat equivalent for the BOLT11 invoice
	// For now, use 1:1 mapping (amount_cbtc = msats)
	// TODO: proper exchange rate
	let amt_msat = amount_cbtc as u64;

	let mut invoice_params: Bolt11InvoiceParameters = Default::default();
	invoice_params.amount_msats = Some(amt_msat);
	invoice_params.invoice_expiry_delta_secs = Some(expiry_secs);
	invoice_params.description = Bolt11InvoiceDescription::Direct(
		Description::new(description.to_string()).expect("description too long for BOLT11"),
	);

	let invoice = match channel_manager.create_bolt11_invoice(invoice_params) {
		Ok(inv) => inv,
		Err(e) => {
			println!("ERROR: failed to create BOLT11 invoice for swap: {:?}", e);
			return None;
		},
	};

	let bolt11 = invoice.to_string();
	let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
	let payment_hash_hex = format!("{}", payment_hash);

	inbound_payments.payments.insert(
		payment_hash,
		PaymentInfo {
			preimage: None,
			secret: Some(invoice.payment_secret().clone()),
			status: HTLCStatus::Pending,
			amt_msat: MillisatAmount(Some(amt_msat)),
		},
	);

	Some((bolt11, payment_hash_hex))
}

pub(crate) fn list_payments(
	inbound_payments: &InboundPaymentInfoStorage, outbound_payments: &OutboundPaymentInfoStorage,
) {
	print!("[");
	for (payment_hash, payment_info) in &inbound_payments.payments {
		println!("");
		println!("\t{{");
		println!("\t\tamount_millisatoshis: {},", payment_info.amt_msat);
		println!("\t\tpayment_hash: {},", payment_hash);
		println!("\t\thtlc_direction: inbound,");
		println!(
			"\t\thtlc_status: {},",
			match payment_info.status {
				HTLCStatus::Pending => "pending",
				HTLCStatus::Succeeded => "succeeded",
				HTLCStatus::Failed => "failed",
			}
		);

		println!("\t}},");
	}

	for (payment_hash, payment_info) in &outbound_payments.payments {
		println!("");
		println!("\t{{");
		println!("\t\tamount_millisatoshis: {},", payment_info.amt_msat);
		println!("\t\tpayment_hash: {},", payment_hash);
		println!("\t\thtlc_direction: outbound,");
		println!(
			"\t\thtlc_status: {},",
			match payment_info.status {
				HTLCStatus::Pending => "pending",
				HTLCStatus::Succeeded => "succeeded",
				HTLCStatus::Failed => "failed",
			}
		);

		println!("\t}},");
	}
	println!("]");
}
