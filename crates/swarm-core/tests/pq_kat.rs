//! Official NIST ACVP known-answer tests, vendored as small subsets in
//! `tests/vectors/pq/`.

use std::path::PathBuf;

use ml_dsa::{
    EncodedVerifyingKey as MlDsaEncodedVerifyingKey, ExpandedSigningKey as MlDsaExpandedSigningKey,
    ExpandedSigningKeyBytes as MlDsaExpandedSigningKeyBytes, MlDsa65, Signature as MlDsaSignature,
    VerifyingKey as MlDsaVerifyingKey,
};
use serde_json::Value;
use slh_dsa::{
    Shake128s, Signature as SlhSignature, SigningKey as SlhSigningKey,
    VerifyingKey as SlhVerifyingKey,
};

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("vectors")
        .join("pq")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("KAT file")).expect("json")
}

fn hex_field(value: &Value, name: &str) -> Vec<u8> {
    let text = value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing {name}"));
    hex::decode(text).expect("hex")
}

#[test]
fn ml_kem_768_acvp() {
    use ml_kem::{Decapsulate as _, Key as MlKemKey, MlKem768, B32};

    let data = load("ml-kem-768.json");
    let mut checked = 0;
    for group in data["testGroups"].as_array().expect("groups") {
        let function = group["function"].as_str().expect("function");
        for test in group["tests"].as_array().expect("tests") {
            match function {
                "encapsulation" => {
                    let ek = hex_field(test, "ek");
                    let m = hex_field(test, "m");
                    let expected_c = hex_field(test, "c");
                    let expected_k = hex_field(test, "k");
                    let key_bytes =
                        MlKemKey::<ml_kem::EncapsulationKey<MlKem768>>::try_from(ek.as_slice())
                            .expect("ek");
                    let key = ml_kem::EncapsulationKey::<MlKem768>::new(&key_bytes).expect("key");
                    let m = B32::try_from(m.as_slice()).expect("m");
                    let (ct, ss) = key.encapsulate_deterministic(&m);
                    assert_eq!(ct.as_slice(), expected_c.as_slice());
                    assert_eq!(ss.as_slice(), expected_k.as_slice());
                }
                "decapsulation" => {
                    #[allow(deprecated)]
                    {
                        let dk = hex_field(test, "dk");
                        let c = hex_field(test, "c");
                        let expected_k = hex_field(test, "k");
                        let expanded =
                            ml_kem::ExpandedDecapsulationKey::<MlKem768>::try_from(dk.as_slice())
                                .expect("expanded dk");
                        let key = ml_kem::DecapsulationKey::<MlKem768>::from_expanded(&expanded)
                            .expect("dk");
                        let ct =
                            ml_kem::Ciphertext::<MlKem768>::try_from(c.as_slice()).expect("ct");
                        let ss = key.decapsulate(&ct);
                        assert_eq!(ss.as_slice(), expected_k.as_slice());
                    }
                }
                other => panic!("unexpected ML-KEM function {other}"),
            }
            checked += 1;
        }
    }
    assert!(checked >= 4, "expected at least four ML-KEM KAT cases");
}

#[test]
fn ml_dsa_65_acvp() {
    let data = load("ml-dsa-65.json");
    let mut checked = 0;
    for group in data["testGroups"].as_array().expect("groups") {
        for test in group["tests"].as_array().expect("tests") {
            let sk = hex_field(test, "sk");
            let pk = hex_field(test, "pk");
            let message = hex_field(test, "message");
            let context = hex_field(test, "context");
            let expected = hex_field(test, "signature");

            #[allow(deprecated)]
            let key_bytes =
                MlDsaExpandedSigningKeyBytes::<MlDsa65>::try_from(sk.as_slice()).expect("sk");
            #[allow(deprecated)]
            let key = MlDsaExpandedSigningKey::<MlDsa65>::from_expanded(&key_bytes);
            let signature = key.sign_deterministic(&message, &context).expect("sign");
            assert_eq!(signature.encode().as_slice(), expected.as_slice());

            let encoded = MlDsaEncodedVerifyingKey::<MlDsa65>::try_from(pk.as_slice()).expect("pk");
            let verifying = MlDsaVerifyingKey::<MlDsa65>::decode(&encoded);
            let parsed = MlDsaSignature::<MlDsa65>::try_from(expected.as_slice()).expect("sig");
            verifying
                .verify_with_context(&message, &context, &parsed)
                .then_some(())
                .expect("verify");
            checked += 1;
        }
    }
    assert!(checked >= 2, "expected at least two ML-DSA KAT cases");
}

#[test]
fn slh_dsa_shake_128s_acvp() {
    let data = load("slh-dsa-shake-128s.json");
    let mut checked = 0;
    for group in data["testGroups"].as_array().expect("groups") {
        for test in group["tests"].as_array().expect("tests") {
            let sk = hex_field(test, "sk");
            let pk = hex_field(test, "pk");
            let message = hex_field(test, "message");
            let context = hex_field(test, "context");
            let expected = hex_field(test, "signature");

            let key = SlhSigningKey::<Shake128s>::try_from(sk.as_slice()).expect("sk");
            let signature = key
                .try_sign_with_context(&message, &context, None)
                .expect("sign");
            assert_eq!(signature.to_vec(), expected);

            let verifying = SlhVerifyingKey::<Shake128s>::try_from(pk.as_slice()).expect("pk");
            let parsed = SlhSignature::<Shake128s>::try_from(expected.as_slice()).expect("sig");
            verifying
                .try_verify_with_context(&message, &context, &parsed)
                .expect("verify");
            checked += 1;
        }
    }
    assert!(checked >= 2, "expected at least two SLH-DSA KAT cases");
}
