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

fn decrypt_with_keyfile_text(text: &str, ciphertext: &[u8], recipient: &str) -> Vec<u8> {
    let (secret, derived) = crypto::select_identity(text, Some(recipient)).expect("select identity");
    assert_eq!(derived, recipient);
    let id = crypto::parse_identity(&secret).unwrap();
    crypto::decrypt_bytes(&id, ciphertext).unwrap()
}

#[test]
fn keyfile_export_import_roundtrip_decrypts_original_data() {
    let (secret, public) = crypto::new_identity();
    let ct = crypto::encrypt_bytes(&crypto::parse_recipient(&public).unwrap(), b"payload").unwrap();

    let body = crypto::format_keyfile(&secret, &public);
    assert!(body.ends_with(&format!("\n{secret}\n")), "secret must be the last line: {body:?}");
    for line in body.lines().filter(|l| !l.starts_with("AGE-SECRET-KEY-")) {
        assert!(line.starts_with('#'), "non-comment line would break `age -i`: {line:?}");
    }
    assert!(!body.contains('\r'));

    let bytes = body.as_bytes().to_vec();
    let text = crypto::decode_keyfile(&bytes).unwrap();
    let (imported, derived) = crypto::select_identity(&text, Some(&public)).unwrap();
    assert_eq!(*imported, secret);
    assert_eq!(derived, public);
    assert_eq!(decrypt_with_keyfile_text(&text, &ct, &public), b"payload");
}

#[test]
fn keyfile_import_tolerates_crlf_bom_whitespace_and_age_keygen_comments() {
    let (secret, public) = crypto::new_identity();
    let ct = crypto::encrypt_bytes(&crypto::parse_recipient(&public).unwrap(), b"x").unwrap();
    let variants = [
        format!("# created: 2026-01-01T00:00:00Z\r\n# public key: {public}\r\n{secret}\r\n"),
        format!("\u{feff}{secret}\n"),
        format!("\n\n   {secret}   \t\n\n"),
        format!("\"secret_identity\": \"{secret}\","),
        secret.clone(),
    ];
    for v in &variants {
        let text = crypto::decode_keyfile(v.as_bytes()).unwrap();
        let (got, _) = crypto::select_identity(&text, Some(&public)).unwrap_or_else(|e| panic!("{v:?}: {e}"));
        assert_eq!(*got, secret, "variant {v:?}");
        assert_eq!(decrypt_with_keyfile_text(&text, &ct, &public), b"x");
    }
}

#[test]
fn keyfile_decode_handles_utf16_with_bom() {
    let (secret, public) = crypto::new_identity();
    let body = format!("# public key: {public}\r\n{secret}\r\n");
    let mut le = vec![0xFF, 0xFE];
    let mut be = vec![0xFE, 0xFF];
    for u in body.encode_utf16() {
        le.extend(u.to_le_bytes());
        be.extend(u.to_be_bytes());
    }
    for bytes in [le, be] {
        let text = crypto::decode_keyfile(&bytes).unwrap();
        let (got, _) = crypto::select_identity(&text, Some(&public)).unwrap();
        assert_eq!(*got, secret);
    }
    assert!(crypto::decode_keyfile(&[0x00, 0xC3, 0x28]).is_err(), "binary must be rejected");
}

#[test]
fn keyfile_import_picks_matching_identity_and_rejects_mismatch() {
    let (other, _) = crypto::new_identity();
    let (secret, public) = crypto::new_identity();
    // Commented-out key is ignored even if it would match.
    let commented = format!("# {secret}\n{other}\n");
    assert!(crypto::select_identity(&commented, Some(&public)).is_err());

    let multi = format!("{other}\n{secret}\n");
    let (got, derived) = crypto::select_identity(&multi, Some(&public)).unwrap();
    assert_eq!(*got, secret);
    assert_eq!(derived, public);

    // No recorded recipient: first valid identity wins.
    let (got, _) = crypto::select_identity(&multi, None).unwrap();
    assert_eq!(*got, other);

    let err = crypto::select_identity(&format!("{other}\n"), Some(&public)).unwrap_err().to_string();
    assert!(err.contains(&public), "mismatch error should name expected recipient: {err}");
    assert!(crypto::select_identity("# only comments\n", None).is_err());
    assert!(crypto::select_identity("AGE-SECRET-KEY-1NOTVALID\n", None).is_err());
}

/// Verifies the exported keyfile against the real `age` CLI when it is on PATH.
#[test]
fn keyfile_is_accepted_by_age_cli() {
    let Ok(out) = std::process::Command::new("age").arg("--version").output() else {
        eprintln!("age CLI not installed; skipping");
        return;
    };
    assert!(out.status.success());
    let (secret, public) = crypto::new_identity();
    let ct = crypto::encrypt_bytes(&crypto::parse_recipient(&public).unwrap(), b"hello age").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("cosmog-key.txt");
    let ct_path = dir.path().join("ct.age");
    std::fs::write(&key, crypto::format_keyfile(&secret, &public)).unwrap();
    std::fs::write(&ct_path, &ct).unwrap();
    let out = std::process::Command::new("age")
        .arg("-d").arg("-i").arg(&key).arg(&ct_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "age stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"hello age");
}
