//! Reading and writing the fields of relay messages.

use std::net::SocketAddr;

use swarm_core::cbor::Value;
use swarm_net::PeerAddr;

use super::{Msg, MAX_INFO_ITEMS};
use crate::RelayError;

pub(super) fn malformed(what: &str) -> RelayError {
    RelayError::Protocol(what.to_string())
}

pub(super) fn fixed<const N: usize>(value: &Value, what: &str) -> Result<[u8; N], RelayError> {
    value
        .as_bytes()
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| malformed(what))
}

pub(super) fn text(value: &Value, what: &str) -> Result<String, RelayError> {
    value
        .as_text()
        .map(str::to_string)
        .ok_or_else(|| malformed(what))
}

pub(super) fn uint(value: &Value, what: &str) -> Result<u64, RelayError> {
    value.as_uint().ok_or_else(|| malformed(what))
}

pub(super) fn flag(value: &Value, what: &str) -> Result<bool, RelayError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        _ => Err(malformed(what)),
    }
}

/// Relay addresses travel as `Peers` entries of kinds 4 and 6.
pub(super) fn addresses(addrs: &[SocketAddr], max: usize) -> Value {
    Value::Array(
        addrs
            .iter()
            .take(max)
            .map(|addr| Value::Bytes(PeerAddr::Tcp(*addr).encode(None)))
            .collect(),
    )
}

pub(super) fn read_addresses(value: &Value, max: usize) -> Result<Vec<SocketAddr>, RelayError> {
    let items = value.as_array().ok_or_else(|| malformed("addresses"))?;
    if items.len() > max {
        return Err(malformed("too many addresses"));
    }
    items
        .iter()
        .map(|item| {
            let bytes = item.as_bytes().ok_or_else(|| malformed("address"))?;
            match PeerAddr::decode(bytes) {
                Ok((PeerAddr::Tcp(addr), None)) => Ok(addr),
                _ => Err(malformed("address")),
            }
        })
        .collect()
}

pub(super) fn read_info(items: &[Value]) -> Result<Msg, RelayError> {
    let caps = items[2].as_array().ok_or_else(|| malformed("caps"))?;
    let limits = items[3].as_map().ok_or_else(|| malformed("limits"))?;
    if caps.len() > MAX_INFO_ITEMS || limits.len() > MAX_INFO_ITEMS {
        return Err(malformed("too many info items"));
    }
    Ok(Msg::InfoReply {
        version: u8::try_from(uint(&items[1], "version")?).map_err(|_| malformed("version"))?,
        caps: caps
            .iter()
            .map(|cap| text(cap, "cap"))
            .collect::<Result<_, _>>()?,
        limits: limits
            .iter()
            .map(|(key, value)| Ok((text(key, "limit")?, uint(value, "limit")?)))
            .collect::<Result<_, RelayError>>()?,
        relay_id: fixed(&items[4], "relay id")?,
        observed: text(&items[5], "observed")?,
    })
}

/// A list of 32-byte identifiers.
pub(super) fn ids(ids: &[[u8; 32]], max: usize) -> Value {
    Value::Array(
        ids.iter()
            .take(max)
            .map(|id| Value::Bytes(id.to_vec()))
            .collect(),
    )
}

pub(super) fn read_ids(value: &Value, max: usize) -> Result<Vec<[u8; 32]>, RelayError> {
    let items = value.as_array().ok_or_else(|| malformed("ids"))?;
    if items.len() > max {
        return Err(malformed("too many ids"));
    }
    items.iter().map(|item| fixed(item, "id")).collect()
}
