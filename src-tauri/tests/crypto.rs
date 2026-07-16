//! Roundtrip + tamper-detection tests for the age-based encryption layer.

use cosmog_lib::crypto;

#[test]
fn identity_generates_bech32_pair() {
    let (secret, public) = crypto::new_identity();
    assert!(secret.starts_with("AGE-SECRET-KEY-"), "secret was {secret:?}");
    assert!(public.starts_with("age1"), "public was {public:?}");
    // Round-trip parse.
    let _id = crypto::parse_identity(&secret).expect("parse secret");
    let _r = crypto::parse_recipient(&public).expect("parse recipient");
}

#[test]
fn encrypt_bytes_roundtrip() {
    let (secret, public) = crypto::new_identity();
    let identity = crypto::parse_identity(&secret).unwrap();
    let recipient = crypto::parse_recipient(&public).unwrap();

    let plaintext = b"the quick brown fox jumps over the lazy dog";
    let ct = crypto::encrypt_bytes(&recipient, plaintext).unwrap();
    assert!(ct.len() > plaintext.len(), "ciphertext should have header + tag");
    assert!(crypto::is_age_ciphertext(&ct), "ciphertext must carry age magic");
    let pt = crypto::decrypt_bytes(&identity, &ct).unwrap();
    assert_eq!(pt, plaintext);
}

#[test]
fn decrypt_with_wrong_identity_fails() {
    let (_s1, pub1) = crypto::new_identity();
    let (s2, _p2) = crypto::new_identity();
    let recipient = crypto::parse_recipient(&pub1).unwrap();
    let wrong = crypto::parse_identity(&s2).unwrap();

    let ct = crypto::encrypt_bytes(&recipient, b"secret").unwrap();
    let err = crypto::decrypt_bytes(&wrong, &ct);
    assert!(err.is_err(), "decrypt with wrong identity must fail");
}

#[test]
fn tamper_detects() {
    let (secret, public) = crypto::new_identity();
    let identity = crypto::parse_identity(&secret).unwrap();
    let recipient = crypto::parse_recipient(&public).unwrap();

    let mut ct = crypto::encrypt_bytes(&recipient, b"authentic payload").unwrap();
    // Flip a byte deep in the payload area (past the header).
    let idx = ct.len() - 8;
    ct[idx] ^= 0xff;
    let err = crypto::decrypt_bytes(&identity, &ct);
    assert!(err.is_err(), "AEAD must detect tampering");
}

#[test]
fn is_age_ciphertext_rejects_plaintext() {
    assert!(!crypto::is_age_ciphertext(b""));
    assert!(!crypto::is_age_ciphertext(b"hello"));
    assert!(!crypto::is_age_ciphertext(b"age-encryption.org/v2\n"));
    assert!(crypto::is_age_ciphertext(b"age-encryption.org/v1\nrest of header"));
}

#[tokio::test]
async fn encrypt_file_streaming_roundtrip() {
    let (secret, public) = crypto::new_identity();
    let identity = crypto::parse_identity(&secret).unwrap();
    let recipient = crypto::parse_recipient(&public).unwrap();

    // 5 MiB of pseudo-random data — exercises multiple 64 KiB chunks.
    let mut plaintext = vec![0u8; 5 * 1024 * 1024];
    for (i, b) in plaintext.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }

    let src = tempfile::NamedTempFile::new().unwrap();
    let ct = tempfile::NamedTempFile::new().unwrap();
    let pt = tempfile::NamedTempFile::new().unwrap();
    tokio::fs::write(src.path(), &plaintext).await.unwrap();

    crypto::encrypt_file(src.path(), ct.path(), recipient).await.unwrap();
    let ct_bytes = tokio::fs::read(ct.path()).await.unwrap();
    assert!(crypto::is_age_ciphertext(&ct_bytes));
    assert!(ct_bytes.len() >= plaintext.len(), "ciphertext must not shrink payload");

    crypto::decrypt_file(ct.path(), pt.path(), identity).await.unwrap();
    let decrypted = tokio::fs::read(pt.path()).await.unwrap();
    assert_eq!(decrypted, plaintext);
}
