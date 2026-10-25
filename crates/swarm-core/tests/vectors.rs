//! Checks the Rust implementation against the independently generated Python
//! vectors in `tests/vectors/`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use swarm_core::cbor;
use swarm_core::crypto;
use swarm_core::ids::{Oid, RepoId};
use swarm_core::manifest::{encode_envelope, Manifest};

fn vector_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("vectors")
        .join(name)
}

fn load(name: &str) -> BTreeMap<String, Vec<u8>> {
    let text = std::fs::read_to_string(vector_path(name)).expect("vector file");
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, value) = line.split_once('=').expect("name = hex");
        let bytes = hex::decode(value.trim()).expect("hex");
        out.insert(name.trim().to_string(), bytes);
    }
    out
}

fn get<'a>(vectors: &'a BTreeMap<String, Vec<u8>>, name: &str) -> &'a [u8] {
    vectors
        .get(name)
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn array32(vectors: &BTreeMap<String, Vec<u8>>, name: &str) -> [u8; 32] {
    get(vectors, name).try_into().expect("32 bytes")
}

#[test]
fn crypto_vectors_match_python() {
    let vectors = load("crypto.hex");

    let bundle = get(&vectors, "suite1_bundle");
    assert_eq!(bundle.len(), 1984);
    assert_eq!(
        crypto::key_id(bundle).as_bytes(),
        get(&vectors, "key_id_suite1_bundle")
    );
    assert_eq!(
        crypto::repo_id(bundle).as_bytes(),
        get(&vectors, "repo_id_equals_key_id")
    );
    assert_eq!(
        crypto::node_id(bundle).as_bytes(),
        get(&vectors, "node_id_equals_key_id")
    );

    let day_bytes: [u8; 8] = get(&vectors, "day").try_into().expect("u64");
    let hour_bytes: [u8; 8] = get(&vectors, "hour").try_into().expect("u64");
    assert_eq!(crypto::day(20_416 * 86_400), u64::from_be_bytes(day_bytes));
    assert_eq!(
        crypto::hour(490_000 * 3_600),
        u64::from_be_bytes(hour_bytes)
    );

    let repo = RepoId::from_bytes(array32(&vectors, "repo"));
    assert_eq!(
        crypto::topic(&repo, 20_416).as_slice(),
        get(&vectors, "topic")
    );

    let hh = array32(&vectors, "hh");
    let ri = array32(&vectors, "ri");
    let rr = array32(&vectors, "rr");
    let ss = array32(&vectors, "ss");

    let interest_key = crypto::interest_key(&repo);
    assert_eq!(interest_key.as_slice(), get(&vectors, "interest_key"));
    assert_eq!(
        crypto::interest(&repo, &hh).as_slice(),
        get(&vectors, "interest")
    );

    let lan_key = crypto::lan_key(&repo);
    assert_eq!(lan_key.as_slice(), get(&vectors, "lan_key"));
    assert_eq!(
        crypto::lan_token(&repo, 490_000).as_slice(),
        get(&vectors, "lan_token")
    );

    let (i2r, r2i) = crypto::session_keys(&hh, &ri, &rr, &ss);
    let mut okm = Vec::with_capacity(64);
    okm.extend_from_slice(&i2r);
    okm.extend_from_slice(&r2i);
    assert_eq!(okm.as_slice(), get(&vectors, "session_okm"));

    let manifest_bytes = get(&vectors, "manifest");
    assert_eq!(
        crypto::sig_input(manifest_bytes).as_slice(),
        get(&vectors, "sig_input")
    );
    assert_eq!(
        crypto::auth_input(1, &hh, get(&vectors, "ek"), get(&vectors, "ct"), &ri, &rr),
        get(&vectors, "auth_input")
    );

    let envelope = encode_envelope(manifest_bytes, &[0u8; 3373]).expect("envelope");
    assert_eq!(
        crypto::manifest_id(&envelope).as_slice(),
        get(&vectors, "manifest_id")
    );
}

#[test]
fn manifest_vector_is_canonical_and_decodes() {
    let vectors = load("crypto.hex");
    let bytes = get(&vectors, "manifest");
    let value = cbor::decode(bytes).expect("canonical CBOR");
    assert_eq!(cbor::encode(&value).expect("encode"), bytes);
    let manifest = Manifest::decode(bytes).expect("manifest");
    assert_eq!(manifest.encode().expect("encode"), bytes);
    assert_eq!(manifest.seq, 2);
    assert_eq!(manifest.refs.len(), 2);
    assert_eq!(manifest.object_format.as_deref(), Some("sha256"));
}

#[test]
fn id_vectors_match_python() {
    let vectors = load("ids.hex");
    let zero = RepoId::from_bytes([0u8; 32]).to_base32();
    assert_eq!(
        zero.as_bytes(),
        get(&vectors, "base32_zero32"),
        "base32 text is stored as hex in the vector file"
    );
    let pattern: Vec<u8> = (0u8..32).collect();
    let pattern_id = RepoId::from_bytes(pattern.clone().try_into().expect("32"));
    assert_eq!(
        pattern_id.to_base32().as_bytes(),
        get(&vectors, "base32_pattern")
    );
    assert_eq!(
        Oid::new(pattern).to_hex().as_bytes(),
        get(&vectors, "hex_pattern")
    );
}
