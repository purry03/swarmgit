//! Canonical CBOR encoding and strict decoding (spec section 5.2).
//!
//! Encoding is written by hand so the bytes never depend on a general-purpose
//! crate's choices. Decoding uses `ciborium`, then re-encodes canonically and
//! compares bytes, which rejects indefinite lengths, non-shortest integers,
//! unsorted maps, duplicate keys, tags, floats and other non-canonical forms.

use thiserror::Error;

/// Largest CBOR input accepted by the generic decoder. The envelope limit is
/// smaller and is enforced before this is called.
pub const MAX_CBOR_BYTES: usize = 16 * 1024 * 1024;

/// A canonical CBOR value. Only the types the schema needs are representable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Unsigned integer.
    Uint(u64),
    /// Byte string.
    Bytes(Vec<u8>),
    /// Text string.
    Text(String),
    /// Array.
    Array(Vec<Value>),
    /// Boolean.
    Bool(bool),
    /// Null, used by wire messages only.
    Null,
    /// Map. Entries are sorted by the encoded key when encoding.
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// Borrow as a `u64` when this is an unsigned integer.
    #[must_use]
    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(n) => Some(*n),
            _ => None,
        }
    }

    /// Borrow as bytes.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// Borrow as text.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow as an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Borrow as a map.
    #[must_use]
    pub fn as_map(&self) -> Option<&[(Value, Value)]> {
        match self {
            Self::Map(m) => Some(m),
            _ => None,
        }
    }
}

/// CBOR errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CborError {
    /// The input is not well-formed CBOR or uses a type outside the schema.
    #[error("malformed CBOR")]
    Malformed,
    /// The input is well-formed but not canonical.
    #[error("non-canonical CBOR")]
    NonCanonical,
    /// A map contains the same encoded key twice.
    #[error("duplicate CBOR map key")]
    DuplicateKey,
    /// The input exceeds the generic CBOR size limit.
    #[error("CBOR input too large")]
    TooLarge,
}

fn encode_head(major: u8, value: u64, out: &mut Vec<u8>) {
    let prefix = major << 5;
    if value < 24 {
        out.push(prefix | u8::try_from(value).unwrap_or(23));
    } else if value <= u64::from(u8::MAX) {
        out.push(prefix | 24);
        out.push(u8::try_from(value).unwrap_or(0));
    } else if value <= u64::from(u16::MAX) {
        out.push(prefix | 25);
        out.extend_from_slice(&u16::try_from(value).unwrap_or(0).to_be_bytes());
    } else if value <= u64::from(u32::MAX) {
        out.push(prefix | 26);
        out.extend_from_slice(&u32::try_from(value).unwrap_or(0).to_be_bytes());
    } else {
        out.push(prefix | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn encode_value(value: &Value, out: &mut Vec<u8>) -> Result<(), CborError> {
    match value {
        Value::Uint(n) => encode_head(0, *n, out),
        Value::Bytes(bytes) => {
            encode_head(
                2,
                u64::try_from(bytes.len()).map_err(|_| CborError::TooLarge)?,
                out,
            );
            out.extend_from_slice(bytes);
        }
        Value::Text(text) => {
            encode_head(
                3,
                u64::try_from(text.len()).map_err(|_| CborError::TooLarge)?,
                out,
            );
            out.extend_from_slice(text.as_bytes());
        }
        Value::Array(items) => {
            encode_head(
                4,
                u64::try_from(items.len()).map_err(|_| CborError::TooLarge)?,
                out,
            );
            for item in items {
                encode_value(item, out)?;
            }
        }
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Null => out.push(0xf6),
        Value::Map(entries) => {
            encode_head(
                5,
                u64::try_from(entries.len()).map_err(|_| CborError::TooLarge)?,
                out,
            );
            let mut encoded = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let mut key_bytes = Vec::new();
                encode_value(key, &mut key_bytes)?;
                encoded.push((key_bytes, value));
            }
            encoded.sort_by(|a, b| a.0.cmp(&b.0));
            for pair in encoded.windows(2) {
                if pair[0].0 == pair[1].0 {
                    return Err(CborError::DuplicateKey);
                }
            }
            for (key_bytes, value) in encoded {
                out.extend_from_slice(&key_bytes);
                encode_value(value, out)?;
            }
        }
    }
    Ok(())
}

/// Encode a value canonically.
pub fn encode(value: &Value) -> Result<Vec<u8>, CborError> {
    let mut out = Vec::new();
    encode_value(value, &mut out)?;
    Ok(out)
}

fn from_ciborium(value: ciborium::Value) -> Result<Value, CborError> {
    match value {
        ciborium::Value::Integer(integer) => {
            let n = u64::try_from(integer).map_err(|_| CborError::Malformed)?;
            Ok(Value::Uint(n))
        }
        ciborium::Value::Bytes(bytes) => Ok(Value::Bytes(bytes)),
        ciborium::Value::Text(text) => Ok(Value::Text(text)),
        ciborium::Value::Array(items) => items
            .into_iter()
            .map(from_ciborium)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ciborium::Value::Bool(value) => Ok(Value::Bool(value)),
        ciborium::Value::Null => Ok(Value::Null),
        ciborium::Value::Map(entries) => entries
            .into_iter()
            .map(|(key, value)| Ok((from_ciborium(key)?, from_ciborium(value)?)))
            .collect::<Result<Vec<_>, CborError>>()
            .map(Value::Map),
        _ => Err(CborError::Malformed),
    }
}

/// Decode bytes and require that they are the canonical encoding of the value.
pub fn decode(bytes: &[u8]) -> Result<Value, CborError> {
    if bytes.len() > MAX_CBOR_BYTES {
        return Err(CborError::TooLarge);
    }
    let parsed: ciborium::Value = ciborium::from_reader(bytes).map_err(|_| CborError::Malformed)?;
    let value = from_ciborium(parsed)?;
    let reencoded = encode(&value).map_err(|err| match err {
        CborError::DuplicateKey => CborError::NonCanonical,
        other => other,
    })?;
    if reencoded != bytes {
        return Err(CborError::NonCanonical);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let value = Value::Map(vec![
            (Value::Uint(15), Value::Uint(1)),
            (Value::Uint(0), Value::Uint(1)),
            (
                Value::Uint(6),
                Value::Map(vec![
                    (
                        Value::Text("refs/heads/main".into()),
                        Value::Bytes(vec![1; 32]),
                    ),
                    (
                        Value::Text("refs/tags/v1".into()),
                        Value::Bytes(vec![2; 32]),
                    ),
                ]),
            ),
        ]);
        let bytes = encode(&value).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(encode(&decoded).unwrap(), bytes);
    }

    #[test]
    fn rejects_non_shortest_integer() {
        assert_eq!(decode(&[0x18, 0x01]), Err(CborError::NonCanonical));
        assert_eq!(decode(&[0x01]).unwrap(), Value::Uint(1));
    }

    #[test]
    fn rejects_indefinite_length() {
        assert_eq!(decode(&[0x9f, 0xff]), Err(CborError::NonCanonical));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let bytes = [0xa2, 0x00, 0x01, 0x00, 0x02];
        assert_eq!(decode(&bytes), Err(CborError::NonCanonical));
        let value = Value::Map(vec![
            (Value::Uint(0), Value::Uint(1)),
            (Value::Uint(0), Value::Uint(2)),
        ]);
        assert_eq!(encode(&value), Err(CborError::DuplicateKey));
    }

    #[test]
    fn rejects_tags_and_floats() {
        assert_eq!(decode(&[0xc0, 0x01]), Err(CborError::Malformed));
        assert_eq!(
            decode(&[0xfb, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(CborError::Malformed)
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        assert_eq!(decode(&[0x01, 0x02]), Err(CborError::NonCanonical));
    }
}
