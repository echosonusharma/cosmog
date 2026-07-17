//! Android-specific HTTPS client for the AWS SDK.
//!
//! `rustls-native-certs` does not know how to read Android's system trust
//! store, so the default aws-smithy connector panics at startup with
//! "TrustStore configured to enable native roots but no valid root certificates
//! parsed!" (see aws-smithy-http-client rustls_provider.rs:159 debug_assert).
//!
//! We ship the Mozilla root store from `webpki-root-certs` and feed it to
//! aws-smithy-http-client's `TrustStore` as a single concatenated PEM blob.

use std::sync::OnceLock;

use aws_smithy_http_client::tls::{rustls_provider::CryptoMode, Provider, TlsContext, TrustStore};
use aws_smithy_http_client::Builder;
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use base64::Engine;

fn bundle_pem() -> &'static [u8] {
    static PEM: OnceLock<Vec<u8>> = OnceLock::new();
    PEM.get_or_init(|| {
        let mut out = String::with_capacity(256 * 1024);
        let engine = base64::engine::general_purpose::STANDARD;
        for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
            out.push_str("-----BEGIN CERTIFICATE-----\n");
            let b64 = engine.encode(der.as_ref());
            for chunk in b64.as_bytes().chunks(64) {
                out.push_str(std::str::from_utf8(chunk).unwrap());
                out.push('\n');
            }
            out.push_str("-----END CERTIFICATE-----\n");
        }
        out.into_bytes()
    })
    .as_slice()
}

pub fn http_client() -> SharedHttpClient {
    static CLIENT: OnceLock<SharedHttpClient> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let trust = TrustStore::empty()
                .with_native_roots(false)
                .with_pem_certificate(bundle_pem().to_vec());
            let ctx = TlsContext::builder()
                .with_trust_store(trust)
                .build()
                .expect("valid TLS context");
            Builder::new()
                .tls_provider(Provider::Rustls(CryptoMode::Ring))
                .tls_context(ctx)
                .build_https()
        })
        .clone()
}
