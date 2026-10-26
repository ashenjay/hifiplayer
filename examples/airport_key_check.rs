//! Validates the built-in AirPort key (structure and RSA consistency).
use base64::Engine;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
fn main() {
    let src = include_str!("../src/airplay/key.rs");
    let body: String = src
        .split("-----BEGIN RSA PRIVATE KEY-----").nth(1).unwrap()
        .split("-----END RSA PRIVATE KEY-----").next().unwrap()
        .chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(&body).expect("base64");
    let key = rsa::RsaPrivateKey::from_pkcs1_der(&der).expect("parse");
    key.validate().expect("valid key");
    println!("PASS key parses and validates: {} bits, e = {}", key.n().bits(), key.e());
}
