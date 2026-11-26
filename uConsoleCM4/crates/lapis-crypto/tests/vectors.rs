//! Frozen cross-language vectors: `tests/vectors/*.json` (also run by Kotlin and Python).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers

use lapis_crypto::ble::{self, Ephemeral, Hello, DIR_C2D};
use lapis_crypto::framing::{self, Reassembler};
use serde_json::Value;
use std::time::Instant;

fn load(name: &str) -> Value {
    let path = format!("{}/../../tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hex(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}

fn arr<const N: usize>(v: &Value) -> [u8; N] {
    hex(v).try_into().unwrap()
}

#[test]
fn chunk_framing_vector() {
    let v = &load("ble_crypto_vectors.json")["chunk_framing_vector"];
    let mtu = v["att_mtu"].as_u64().unwrap() as usize;
    assert_eq!(
        framing::max_chunk_data(mtu),
        v["max_chunk_data"].as_u64().unwrap() as usize
    );
    let payload = hex(&v["payload_hex"]);
    let frames = framing::split(
        v["msg_id"].as_u64().unwrap() as u8,
        v["flags"].as_u64().unwrap() as u8,
        &payload,
        framing::max_chunk_data(mtu),
    )
    .unwrap();
    let expected: Vec<Vec<u8>> = v["expected_frames_hex"]
        .as_array()
        .unwrap()
        .iter()
        .map(hex)
        .collect();
    assert_eq!(frames, expected);
    let mut rx = Reassembler::default();
    let now = Instant::now();
    assert_eq!(rx.feed(&frames[0], now).unwrap(), None);
    assert_eq!(rx.feed(&frames[1], now).unwrap(), Some((1, payload)));
}

#[test]
fn crypto_vector() {
    let v = &load("ble_crypto_vectors.json")["crypto_vector"];
    let d = &v["derived"];
    let client = Ephemeral::from_parts(arr(&v["client_priv_hex"]), arr(&v["client_nonce_hex"]));
    let device = Ephemeral::from_parts(arr(&v["device_priv_hex"]), arr(&v["device_nonce_hex"]));
    assert_eq!(client.hello.public.to_vec(), hex(&v["client_pub_hex"]));
    assert_eq!(device.hello.public.to_vec(), hex(&v["device_pub_hex"]));
    let shared = client.shared(&device.hello.public).unwrap();
    assert_eq!(shared, device.shared(&client.hello.public).unwrap());
    assert_eq!(shared.to_vec(), hex(&v["shared_secret_hex"]));

    // Hellos survive the wire.
    let c_hello = Hello::decode(&client.hello.encode()).unwrap();
    assert_eq!(c_hello, client.hello);

    let pin = v["pin"].as_str().unwrap();
    let t = ble::transcript(&client.hello, &device.hello);
    assert_eq!(ble::salt(&t, pin).to_vec(), hex(&d["salt_hex"]));
    let keys = ble::derive(&shared, &client.hello, &device.hello, pin);
    assert_eq!(keys.c2d.to_vec(), hex(&d["k_c2d_hex"]));
    assert_eq!(keys.d2c.to_vec(), hex(&d["k_d2c_hex"]));
    assert_eq!(keys.pin.to_vec(), hex(&d["k_pin_hex"]));
    assert_eq!(keys.client_proof().to_vec(), hex(&d["client_proof_hex"]));
    assert_eq!(keys.server_proof().to_vec(), hex(&d["server_proof_hex"]));
    assert!(keys.verify_client_proof(&hex(&d["client_proof_hex"])));
    assert!(!keys.verify_client_proof(&hex(&d["server_proof_hex"])));

    let seq = v["seq"].as_u64().unwrap() as u32;
    let dir = v["dir_byte"].as_u64().unwrap() as u8;
    assert_eq!(dir, DIR_C2D);
    assert_eq!(ble::nonce(dir, seq).to_vec(), hex(&d["nonce_hex"]));
    assert_eq!(ble::aad(dir, seq).to_vec(), hex(&d["aad_hex"]));
    let plaintext = v["plaintext_utf8"].as_str().unwrap().as_bytes();
    let envelope = ble::seal(&keys.c2d, dir, seq, plaintext).unwrap();
    assert_eq!(envelope, hex(&d["envelope_hex"]));
    assert_eq!(
        ble::open(&keys.c2d, dir, &envelope, seq).unwrap(),
        plaintext
    );

    // Replay, wrong key, wrong PIN and tampering are all refused.
    assert!(ble::open(&keys.c2d, dir, &envelope, seq + 1).is_err());
    assert!(ble::open(&keys.d2c, dir, &envelope, seq).is_err());
    let wrong = ble::derive(&shared, &client.hello, &device.hello, "000000");
    assert!(ble::open(&wrong.c2d, dir, &envelope, seq).is_err());
    let mut tampered = envelope.clone();
    tampered[6] ^= 1;
    assert_eq!(
        ble::open(&keys.c2d, dir, &tampered, seq),
        Err(ble::CryptoError::Auth)
    );

    // Channels: seq advances only on success.
    let mut dev = ble::DeviceChannel::new(keys.clone());
    let mut phone = ble::ClientChannel::new(keys);
    let first = phone.seal(b"{\"cmd\":\"wifi_scan\"}").unwrap();
    assert_eq!(first, envelope);
    assert!(dev.open(&tampered).is_err());
    assert_eq!(dev.expected_rx(), 1);
    assert_eq!(dev.open(&first).unwrap(), plaintext);
    assert!(dev.open(&first).is_err(), "replay");
    let reply = dev.seal(b"{\"evt\":\"bye\"}").unwrap();
    assert_eq!(phone.open(&reply).unwrap(), b"{\"evt\":\"bye\"}");
}

#[test]
fn handshake_rejections() {
    assert_eq!(Hello::decode(&[1; 48]), Err(ble::CryptoError::HelloLength));
    let mut bad = [0u8; 49];
    bad[0] = 2;
    assert_eq!(Hello::decode(&bad), Err(ble::CryptoError::Version(2)));
    // A low-order point (all zero) yields the all-zero secret, which is refused.
    let e = Ephemeral::generate();
    assert_eq!(e.shared(&[0u8; 32]), Err(ble::CryptoError::ZeroShared));
}

#[test]
fn redaction_vectors() {
    let v = load("redaction_vectors.json");
    for case in v["text_cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        assert_eq!(
            lapis_crypto::redact(input),
            case["expected"].as_str().unwrap(),
            "{input}"
        );
    }
    for case in v["json_cases"].as_array().unwrap() {
        let mut value = case["input"].clone();
        lapis_crypto::redact_json(&mut value);
        assert_eq!(value, case["expected"], "{}", case["input"]);
    }
}
