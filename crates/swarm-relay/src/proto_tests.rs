use super::*;

fn samples() -> Vec<Msg> {
    vec![
        Msg::HelloAuth {
            version: VERSION,
            role: Role::Connect,
            password: "secret".into(),
        },
        Msg::Hello {
            version: VERSION,
            role: Role::Listen,
        },
        Msg::Error {
            code: "no_such_node".into(),
            msg: "x".into(),
        },
        Msg::Challenge { nonce: [7; 32] },
        Msg::Prove {
            bundle: vec![1; 10],
            sig: vec![2; 10],
            name: "relay:1".into(),
        },
        Msg::Listening {
            ttl_secs: 75,
            observed: "1.2.3.4:5".into(),
        },
        Msg::Incoming { token: [9; 16] },
        Msg::Connect { target: [3; 32] },
        Msg::Accept { token: [4; 16] },
        Msg::Ready,
        Msg::Ping { nonce: 1 },
        Msg::Pong { nonce: 1 },
        Msg::Info,
        Msg::InfoReply {
            version: VERSION,
            caps: vec!["listen".into()],
            limits: vec![("max_splices".into(), 500), ("idle_secs".into(), 120)],
            relay_id: [5; 32],
            observed: "[2001:db8::1]:9".into(),
        },
        Msg::GetRelays { max: 32 },
        Msg::Relays {
            addrs: vec![
                "192.0.2.1:42425".parse().unwrap(),
                "[2001:db8::2]:1".parse().unwrap(),
            ],
        },
        Msg::Where { target: [6; 32] },
        Msg::Publish {
            topics: vec![[1; 32], [2; 32]],
        },
        Msg::Published { ttl_secs: 3600 },
        Msg::Lookup { topic: [8; 32] },
        Msg::Holders {
            nodes: vec![[3; 32]],
        },
        Msg::Referral {
            found: false,
            closer: vec!["192.0.2.3:7".parse().unwrap()],
        },
        Msg::DialBack {
            port: 42425,
            relay_id: [7; 32],
        },
        Msg::DialBackReply { reachable: true },
    ]
}

#[test]
fn every_message_round_trips() {
    for msg in samples() {
        let bytes = msg.encode().unwrap();
        let decoded = Msg::decode(&bytes).unwrap();
        // Limits come back in canonical map order.
        assert_eq!(decoded.encode().unwrap(), bytes);
        if !matches!(msg, Msg::InfoReply { .. }) {
            assert_eq!(decoded, msg);
        }
    }
}

#[test]
fn rejects_unknown_type_and_bad_lengths() {
    let bytes = cbor::encode(&Value::Array(vec![Value::Uint(99)])).unwrap();
    assert!(Msg::decode(&bytes).is_err());
    let bytes = cbor::encode(&Value::Array(vec![
        Value::Uint(5),
        Value::Bytes(vec![0; 15]),
    ]))
    .unwrap();
    assert!(Msg::decode(&bytes).is_err());
    let bytes = cbor::encode(&Value::Array(vec![Value::Uint(8), Value::Uint(1)])).unwrap();
    assert!(Msg::decode(&bytes).is_err());
    assert!(Msg::decode(b"\xff").is_err());
    let many = Msg::Relays {
        addrs: vec!["192.0.2.1:1".parse().unwrap(); MAX_RELAYS_REPLY],
    };
    assert!(many.encode().unwrap().len() < MAX_FRAME);
    let named = cbor::encode(&Value::Array(vec![
        Value::Uint(14),
        Value::Array(vec![Value::Bytes(vec![5; 39])]),
    ]))
    .unwrap();
    assert!(Msg::decode(&named).is_err());
}

#[tokio::test]
async fn frame_limit_is_enforced_and_no_read_ahead() {
    let (mut a, mut b) = tokio::io::duplex(1 << 16);
    let mut huge = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
    huge.extend(vec![0u8; 16]);
    a.write_all(&huge).await.unwrap();
    assert!(read_msg(&mut b).await.is_err());

    let (mut a, mut b) = tokio::io::duplex(1 << 16);
    write_msg(&mut a, &Msg::Ready).await.unwrap();
    a.write_all(b"RAW").await.unwrap();
    assert_eq!(read_msg(&mut b).await.unwrap(), Msg::Ready);
    let mut rest = [0u8; 3];
    b.read_exact(&mut rest).await.unwrap();
    assert_eq!(&rest, b"RAW");
}
