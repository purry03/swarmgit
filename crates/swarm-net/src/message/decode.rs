//! Decoding for the CBOR wire messages (spec section 7.3).

use swarm_core::cbor::{self, Value};
use swarm_core::consts::MAX_ABOUT_BYTES;
use swarm_core::ids::Oid;

use super::{Message, MrTip, MAX_MR_TIPS};
use crate::NetError;

impl Message {
    /// Decode one canonical message.
    pub fn decode(bytes: &[u8]) -> Result<Self, NetError> {
        let value = cbor::decode(bytes)?;
        let items = value
            .as_array()
            .ok_or_else(|| NetError::Malformed("message is not an array".into()))?;
        let kind = items
            .first()
            .and_then(Value::as_uint)
            .ok_or_else(|| NetError::Malformed("message has no type".into()))?;
        match kind {
            0 => decode_hello(items),
            1 => decode_error(items),
            2 => decode_get_heads(items),
            3 => decode_heads(items),
            4 => decode_get_manifest(items),
            5 => decode_manifest(items),
            6 => decode_announce(items),
            7 => decode_get_peers(items),
            8 => decode_peers(items),
            9 => decode_want(items),
            10 => decode_pack_header(items),
            11 => decode_pack_data(items),
            12 => decode_ping(items),
            13 => decode_pong(items),
            14 => decode_pq_init(items),
            15 => decode_pq_resp(items),
            16 => decode_pq_auth(items),
            17 => decode_punch_offer(items),
            18 => decode_punch_answer(items),
            19 => decode_get_about(items),
            20 => decode_about(items),
            21 => decode_announce_about(items),
            22 => decode_get_mrs(items),
            23 => decode_mrs(items),
            24 => decode_announce_mrs(items),
            other => Err(NetError::Malformed(format!("unknown message type {other}"))),
        }
    }
}

fn about_record(value: &Value) -> Result<Vec<u8>, NetError> {
    let record = as_bytes(value)?;
    if record.len() > MAX_ABOUT_BYTES {
        return Err(NetError::LimitExceeded("about record is too large".into()));
    }
    Ok(record.to_vec())
}

fn decode_get_about(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::GetAbout {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
    })
}

fn decode_about(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    let record = match &items[2] {
        Value::Null => None,
        value => Some(about_record(value)?),
    };
    Ok(Message::About {
        rid: as_u64(&items[1])?,
        record,
    })
}

fn decode_announce_about(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::AnnounceAbout {
        repo_ix: as_u16(&items[1])?,
        record: about_record(&items[2])?,
    })
}

fn mr_tips(value: &Value) -> Result<Vec<MrTip>, NetError> {
    let items = value
        .as_array()
        .ok_or_else(|| NetError::Malformed("expected an array".into()))?;
    if items.len() > MAX_MR_TIPS {
        return Err(NetError::LimitExceeded("too many merge requests".into()));
    }
    items
        .iter()
        .map(|item| {
            let parts = item
                .as_array()
                .filter(|parts| parts.len() == 3)
                .ok_or_else(|| NetError::Malformed("bad merge request entry".into()))?;
            Ok(MrTip {
                mr: as_oid(&parts[0])?,
                author: as_array32(&parts[1])?,
                tip: as_oid(&parts[2])?,
            })
        })
        .collect()
}

fn decode_get_mrs(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::GetMrs {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
    })
}

fn decode_mrs(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::Mrs {
        rid: as_u64(&items[1])?,
        tips: mr_tips(&items[2])?,
    })
}

fn decode_announce_mrs(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::AnnounceMrs {
        repo_ix: as_u16(&items[1])?,
        tips: mr_tips(&items[2])?,
    })
}

/// Most candidate addresses a punch message may list.
const MAX_PUNCH_ADDRESSES: usize = 8;

fn punch_addresses(value: &Value) -> Result<Vec<Vec<u8>>, NetError> {
    let addresses = as_bytes_vec(value)?;
    if addresses.len() > MAX_PUNCH_ADDRESSES {
        return Err(NetError::LimitExceeded("too many punch addresses".into()));
    }
    Ok(addresses)
}

fn decode_punch_offer(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 4)?;
    let token = <[u8; 16]>::try_from(as_bytes(&items[2])?)
        .map_err(|_| NetError::Malformed("punch token is not 16 bytes".into()))?;
    Ok(Message::PunchOffer {
        rid: as_u64(&items[1])?,
        token,
        addresses: punch_addresses(&items[3])?,
    })
}

fn decode_punch_answer(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::PunchAnswer {
        rid: as_u64(&items[1])?,
        addresses: punch_addresses(&items[2])?,
    })
}

fn decode_hello(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 5)?;
    let interests = as_bytes_vec(&items[4])?;
    if interests.len() > 16 {
        return Err(NetError::LimitExceeded("too many interests".into()));
    }
    for token in &interests {
        if token.len() != 16 {
            return Err(NetError::Malformed("interest is not 16 bytes".into()));
        }
    }
    Ok(Message::Hello {
        version: as_u8(&items[1])?,
        caps: as_text_vec(&items[2])?,
        observed: as_bytes(&items[3])?.to_vec(),
        interests,
    })
}

fn decode_error(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 4)?;
    Ok(Message::Error {
        rid: as_u64(&items[1])?,
        code: as_text(&items[2])?.to_string(),
        msg: as_text(&items[3])?.to_string(),
    })
}

fn decode_get_heads(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::GetHeads {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
    })
}

fn decode_heads(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    let envelopes = as_bytes_vec(&items[2])?;
    if envelopes.len() > 65 {
        return Err(NetError::LimitExceeded("too many heads".into()));
    }
    Ok(Message::Heads {
        rid: as_u64(&items[1])?,
        envelopes,
    })
}

fn decode_get_manifest(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 5)?;
    Ok(Message::GetManifest {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
        signer: as_array32(&items[3])?,
        min_seq: as_u64(&items[4])?,
    })
}

fn decode_manifest(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    let envelope = match &items[2] {
        Value::Null => None,
        Value::Bytes(bytes) => Some(bytes.clone()),
        _ => return Err(NetError::Malformed("manifest reply is not bytes".into())),
    };
    Ok(Message::Manifest {
        rid: as_u64(&items[1])?,
        envelope,
    })
}

fn decode_announce(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::Announce {
        repo_ix: as_u16(&items[1])?,
        envelope: as_bytes(&items[2])?.to_vec(),
    })
}

fn decode_get_peers(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::GetPeers {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
    })
}

fn decode_peers(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    let addresses = as_bytes_vec(&items[2])?;
    if addresses.len() > 32 {
        return Err(NetError::LimitExceeded("too many peers".into()));
    }
    Ok(Message::Peers {
        rid: as_u64(&items[1])?,
        addresses,
    })
}

fn decode_want(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 5)?;
    let wants = as_oids(&items[3])?;
    let haves = as_oids(&items[4])?;
    if wants.len() > 4096 || haves.len() > 1024 {
        return Err(NetError::LimitExceeded("too many wants or haves".into()));
    }
    Ok(Message::Want {
        rid: as_u64(&items[1])?,
        repo_ix: as_u16(&items[2])?,
        wants,
        haves,
    })
}

fn decode_pack_header(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    let size_hint = match &items[2] {
        Value::Null => None,
        other => Some(as_u64(other)?),
    };
    Ok(Message::PackHeader {
        rid: as_u64(&items[1])?,
        size_hint,
    })
}

fn decode_pack_data(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 4)?;
    let final_chunk = match &items[3] {
        Value::Bool(value) => *value,
        _ => return Err(NetError::Malformed("pack final flag is not bool".into())),
    };
    Ok(Message::PackData {
        rid: as_u64(&items[1])?,
        chunk: as_bytes(&items[2])?.to_vec(),
        final_chunk,
    })
}

fn decode_ping(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 2)?;
    Ok(Message::Ping {
        nonce: as_u64(&items[1])?,
    })
}

fn decode_pong(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 2)?;
    Ok(Message::Pong {
        nonce: as_u64(&items[1])?,
    })
}

fn decode_pq_init(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 4)?;
    let pad = as_bytes(&items[3])?;
    if pad.len() > 255 {
        return Err(NetError::LimitExceeded("pq padding is too large".into()));
    }
    Ok(Message::PqInit {
        ek: as_bytes(&items[1])?.to_vec(),
        r_i: as_array32(&items[2])?,
        pad: pad.to_vec(),
    })
}

fn decode_pq_resp(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 4)?;
    let pad = as_bytes(&items[3])?;
    if pad.len() > 255 {
        return Err(NetError::LimitExceeded("pq padding is too large".into()));
    }
    Ok(Message::PqResp {
        ct: as_bytes(&items[1])?.to_vec(),
        r_r: as_array32(&items[2])?,
        pad: pad.to_vec(),
    })
}

fn decode_pq_auth(items: &[Value]) -> Result<Message, NetError> {
    expect_len(items, 3)?;
    Ok(Message::PqAuth {
        bundle: as_bytes(&items[1])?.to_vec(),
        sig: as_bytes(&items[2])?.to_vec(),
    })
}

fn expect_len(items: &[Value], len: usize) -> Result<(), NetError> {
    if items.len() == len {
        Ok(())
    } else {
        Err(NetError::Malformed(format!(
            "message has {} fields, want {len}",
            items.len()
        )))
    }
}

fn as_u64(value: &Value) -> Result<u64, NetError> {
    value
        .as_uint()
        .ok_or_else(|| NetError::Malformed("expected an unsigned integer".into()))
}

fn as_u16(value: &Value) -> Result<u16, NetError> {
    u16::try_from(as_u64(value)?).map_err(|_| NetError::Malformed("integer is too large".into()))
}

fn as_u8(value: &Value) -> Result<u8, NetError> {
    u8::try_from(as_u64(value)?).map_err(|_| NetError::Malformed("integer is too large".into()))
}

fn as_bytes(value: &Value) -> Result<&[u8], NetError> {
    value
        .as_bytes()
        .ok_or_else(|| NetError::Malformed("expected a byte string".into()))
}

fn as_text(value: &Value) -> Result<&str, NetError> {
    value
        .as_text()
        .ok_or_else(|| NetError::Malformed("expected a text string".into()))
}

fn as_array32(value: &Value) -> Result<[u8; 32], NetError> {
    as_bytes(value)?
        .try_into()
        .map_err(|_| NetError::Malformed("expected 32 bytes".into()))
}

fn as_bytes_vec(value: &Value) -> Result<Vec<Vec<u8>>, NetError> {
    let items = value
        .as_array()
        .ok_or_else(|| NetError::Malformed("expected an array".into()))?;
    items
        .iter()
        .map(|item| Ok(as_bytes(item)?.to_vec()))
        .collect()
}

fn as_text_vec(value: &Value) -> Result<Vec<String>, NetError> {
    let items = value
        .as_array()
        .ok_or_else(|| NetError::Malformed("expected an array".into()))?;
    items
        .iter()
        .map(|item| Ok(as_text(item)?.to_string()))
        .collect()
}

fn as_oids(value: &Value) -> Result<Vec<Oid>, NetError> {
    let items = value
        .as_array()
        .ok_or_else(|| NetError::Malformed("expected an array".into()))?;
    items.iter().map(as_oid).collect()
}

fn as_oid(value: &Value) -> Result<Oid, NetError> {
    let bytes = as_bytes(value)?;
    if bytes.len() != 20 && bytes.len() != 32 {
        return Err(NetError::Malformed(
            "object ID is not 20 or 32 bytes".into(),
        ));
    }
    Ok(Oid::new(bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use swarm_core::cbor::Value;

    fn round_trip(message: Message) {
        let bytes = message.encode().unwrap();
        assert_eq!(Message::decode(&bytes).unwrap(), message);
        assert_eq!(
            Message::decode(&bytes).unwrap().type_number(),
            message.type_number()
        );
    }

    #[test]
    fn all_message_types_round_trip() {
        let oid = Oid::new(vec![3u8; 32]);
        round_trip(Message::Hello {
            version: 1,
            caps: vec!["want".into()],
            observed: vec![4, 127, 0, 0, 1, 0x10, 0x01],
            interests: vec![vec![1u8; 16]],
        });
        round_trip(Message::Error {
            rid: 4,
            code: "stale".into(),
            msg: "old".into(),
        });
        round_trip(Message::GetHeads { rid: 1, repo_ix: 0 });
        round_trip(Message::Heads {
            rid: 1,
            envelopes: vec![vec![1, 2, 3]],
        });
        round_trip(Message::GetManifest {
            rid: 2,
            repo_ix: 0,
            signer: [9u8; 32],
            min_seq: 3,
        });
        round_trip(Message::Manifest {
            rid: 2,
            envelope: None,
        });
        round_trip(Message::Announce {
            repo_ix: 0,
            envelope: vec![4, 5],
        });
        round_trip(Message::GetPeers { rid: 3, repo_ix: 0 });
        round_trip(Message::Peers {
            rid: 3,
            addresses: vec![vec![4, 127, 0, 0, 1, 0, 1]],
        });
        round_trip(Message::Want {
            rid: 5,
            repo_ix: 0,
            wants: vec![oid],
            haves: vec![],
        });
        round_trip(Message::PackHeader {
            rid: 5,
            size_hint: Some(10),
        });
        round_trip(Message::PackData {
            rid: 5,
            chunk: vec![1, 2],
            final_chunk: true,
        });
        let tip = MrTip {
            mr: Oid::new(vec![1u8; 20]),
            author: [2u8; 32],
            tip: Oid::new(vec![3u8; 20]),
        };
        round_trip(Message::GetMrs { rid: 6, repo_ix: 0 });
        round_trip(Message::Mrs {
            rid: 6,
            tips: vec![tip.clone()],
        });
        round_trip(Message::AnnounceMrs {
            repo_ix: 0,
            tips: vec![tip],
        });
        round_trip(Message::Ping { nonce: 8 });
        round_trip(Message::Pong { nonce: 8 });
        round_trip(Message::PqInit {
            ek: vec![1; 1184],
            r_i: [2; 32],
            pad: vec![0; 7],
        });
        round_trip(Message::PqResp {
            ct: vec![3; 1088],
            r_r: [4; 32],
            pad: vec![],
        });
        round_trip(Message::PqAuth {
            bundle: vec![5; 1984],
            sig: vec![6; 3373],
        });
    }

    #[test]
    fn rejects_unknown_types_and_wrong_field_counts() {
        assert!(Message::decode(&{
            let mut bytes = cbor::encode(&Value::Array(vec![Value::Uint(99)])).unwrap();
            bytes.shrink_to_fit();
            bytes
        })
        .is_err());
        let bytes = cbor::encode(&Value::Array(vec![Value::Uint(12)])).unwrap();
        assert!(Message::decode(&bytes).is_err());
    }

    #[test]
    fn rejects_non_canonical_bytes() {
        // Indefinite-length array.
        assert!(Message::decode(&[0x9f, 0x0c, 0x01, 0xff]).is_err());
    }
}
