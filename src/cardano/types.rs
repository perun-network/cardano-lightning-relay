//! Cardano Lightning Liquidity Manager contract types.
//!
//! Parses Plutus JSON datum format returned by Blockfrost's inline_datum field.
//! Also handles CBOR hex strings returned by yaci-devkit's Blockfrost-compatible API.
//! The datum structure matches the Aiken `State` and `Invoice` types from the
//! lightning-liquidity-manager contract.

use ciborium::value::Value as CborValue;
use serde_json::Value;
use std::fmt;

/// Contract pool state (top-level datum).
#[derive(Debug, Clone)]
pub struct State {
    pub total_liquidity: i64,
    pub reserved: i64,
    pub last_invoice_id: i64,
    pub invoices: Vec<Invoice>,
}

/// A single invoice within the contract state.
#[derive(Debug, Clone)]
pub struct Invoice {
    pub invoice_id: i64,
    pub amount: i64,
    pub owner: String, // hex-encoded payment key hash
    pub timestamp: i64,
    pub expires_at: i64,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let available = self.total_liquidity - self.reserved;
        writeln!(f, "Pool State:")?;
        writeln!(f, "  total_liquidity: {}", self.total_liquidity)?;
        writeln!(f, "  reserved:        {}", self.reserved)?;
        writeln!(f, "  available:        {}", available)?;
        writeln!(f, "  last_invoice_id: {}", self.last_invoice_id)?;
        writeln!(f, "  invoices:        {} active", self.invoices.len())?;
        for inv in &self.invoices {
            writeln!(f, "    - {}", inv)?;
        }
        Ok(())
    }
}

impl fmt::Display for Invoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invoice #{}: amount={}, owner={}, ts={}, expires={}",
            self.invoice_id, self.amount, self.owner, self.timestamp, self.expires_at,
        )
    }
}

/// Extract an integer from a Plutus JSON field like `{"int": 42}`.
fn plutus_int(val: &Value) -> Result<i64, String> {
    val.get("int")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("expected {{\"int\": ...}}, got {}", val))
}

/// Extract a byte string from a Plutus JSON field like `{"bytes": "abcd"}`.
fn plutus_bytes(val: &Value) -> Result<String, String> {
    val.get("bytes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("expected {{\"bytes\": ...}}, got {}", val))
}

/// Convert a CBOR hex string (as returned by yaci-devkit) into Plutus JSON format.
///
/// Plutus CBOR encoding:
/// - Tag 121..127 → Constructor 0..6 with fields
/// - Tag 102 → Constructor N > 6 with [N, fields]
/// - Integer → `{"int": N}`
/// - Bytes → `{"bytes": "hex"}`
/// - Array → `{"list": [...]}`
pub fn cbor_hex_to_plutus_json(hex_str: &str) -> Result<Value, String> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| format!("invalid hex in inline_datum: {}", e))?;
    let cbor: CborValue = ciborium::de::from_reader(&bytes[..])
        .map_err(|e| format!("CBOR decode failed: {}", e))?;
    cbor_value_to_plutus_json(&cbor)
}

fn cbor_value_to_plutus_json(val: &CborValue) -> Result<Value, String> {
    match val {
        CborValue::Integer(n) => {
            let n: i128 = (*n).into();
            Ok(serde_json::json!({"int": n as i64}))
        }
        CborValue::Bytes(b) => {
            Ok(serde_json::json!({"bytes": hex::encode(b)}))
        }
        CborValue::Array(items) => {
            let converted: Result<Vec<Value>, String> =
                items.iter().map(cbor_value_to_plutus_json).collect();
            Ok(serde_json::json!({"list": converted?}))
        }
        CborValue::Tag(tag, inner) => {
            // Plutus constructor encoding: tags 121-127 = constructors 0-6
            let constructor = if (121..=127).contains(tag) {
                *tag - 121
            } else if *tag == 102 {
                // Alternative encoding: Tag 102 with [constructor_index, fields]
                if let CborValue::Array(arr) = inner.as_ref() {
                    if let Some(CborValue::Integer(n)) = arr.first() {
                        let n: i128 = (*n).into();
                        let fields: Result<Vec<Value>, String> =
                            arr[1..].iter().flat_map(|item| {
                                if let CborValue::Array(inner_fields) = item {
                                    inner_fields.iter().map(cbor_value_to_plutus_json).collect()
                                } else {
                                    vec![cbor_value_to_plutus_json(item)]
                                }
                            }).collect();
                        return Ok(serde_json::json!({
                            "constructor": n as i64,
                            "fields": fields?
                        }));
                    }
                }
                return Err(format!("invalid Tag 102 encoding: {:?}", inner));
            } else {
                return Err(format!("unexpected CBOR tag: {}", tag));
            };

            // Tags 121-127: inner value is the fields array
            let fields = match inner.as_ref() {
                CborValue::Array(items) => {
                    let converted: Result<Vec<Value>, String> =
                        items.iter().map(cbor_value_to_plutus_json).collect();
                    converted?
                }
                _ => return Err(format!("expected array inside constructor tag, got {:?}", inner)),
            };

            Ok(serde_json::json!({
                "constructor": constructor,
                "fields": fields
            }))
        }
        CborValue::Text(s) => {
            Ok(serde_json::json!({"bytes": hex::encode(s.as_bytes())}))
        }
        other => Err(format!("unsupported CBOR value type: {:?}", other)),
    }
}

impl TryFrom<&Value> for Invoice {
    type Error = String;

    /// Parse from Plutus JSON:
    /// ```json
    /// {"constructor": 0, "fields": [
    ///   {"int": invoice_id},
    ///   {"int": amount},
    ///   {"bytes": "owner_pkh_hex"},
    ///   {"int": timestamp},
    ///   {"int": expires_at}
    /// ]}
    /// ```
    fn try_from(val: &Value) -> Result<Self, String> {
        let fields = val
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or("Invoice: missing 'fields' array")?;

        if fields.len() < 5 {
            return Err(format!("Invoice: expected 5 fields, got {}", fields.len()));
        }

        Ok(Invoice {
            invoice_id: plutus_int(&fields[0])?,
            amount: plutus_int(&fields[1])?,
            owner: plutus_bytes(&fields[2])?,
            timestamp: plutus_int(&fields[3])?,
            expires_at: plutus_int(&fields[4])?,
        })
    }
}

impl TryFrom<&Value> for State {
    type Error = String;

    /// Parse from Plutus JSON:
    /// ```json
    /// {"constructor": 0, "fields": [
    ///   {"int": total_liquidity},
    ///   {"int": reserved},
    ///   {"int": last_invoice_id},
    ///   {"list": [ ...invoices... ]}
    /// ]}
    /// ```
    fn try_from(val: &Value) -> Result<Self, String> {
        let fields = val
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or("State: missing 'fields' array")?;

        if fields.len() < 4 {
            return Err(format!("State: expected 4 fields, got {}", fields.len()));
        }

        let invoices_list = fields[3]
            .get("list")
            .and_then(|l| l.as_array())
            .ok_or("State: field 3 should be a list of invoices")?;

        let invoices: Result<Vec<Invoice>, String> =
            invoices_list.iter().map(Invoice::try_from).collect();

        Ok(State {
            total_liquidity: plutus_int(&fields[0])?,
            reserved: plutus_int(&fields[1])?,
            last_invoice_id: plutus_int(&fields[2])?,
            invoices: invoices?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_empty_state() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"int": 1_000_000},
                {"int": 0},
                {"int": 0},
                {"list": []}
            ]
        });
        let state = State::try_from(&datum).unwrap();
        assert_eq!(state.total_liquidity, 1_000_000);
        assert_eq!(state.reserved, 0);
        assert_eq!(state.last_invoice_id, 0);
        assert!(state.invoices.is_empty());
    }

    #[test]
    fn parse_cbor_hex_state() {
        // d8799f1a000f424000009fffff = CBORTag(121, [1000000, 0, 0, []])
        let plutus_json = cbor_hex_to_plutus_json("d8799f1a000f424000009fffff").unwrap();
        let state = State::try_from(&plutus_json).unwrap();
        assert_eq!(state.total_liquidity, 1_000_000);
        assert_eq!(state.reserved, 0);
        assert_eq!(state.last_invoice_id, 0);
        assert!(state.invoices.is_empty());
    }

    #[test]
    fn parse_state_with_invoice() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"int": 5_000_000},
                {"int": 100_000},
                {"int": 1},
                {"list": [{
                    "constructor": 0,
                    "fields": [
                        {"int": 1},
                        {"int": 100_000},
                        {"bytes": "abcdef0123456789"},
                        {"int": 1700000000000_i64},
                        {"int": 1700003600000_i64}
                    ]
                }]}
            ]
        });
        let state = State::try_from(&datum).unwrap();
        assert_eq!(state.total_liquidity, 5_000_000);
        assert_eq!(state.reserved, 100_000);
        assert_eq!(state.invoices.len(), 1);
        assert_eq!(state.invoices[0].owner, "abcdef0123456789");
    }
}
