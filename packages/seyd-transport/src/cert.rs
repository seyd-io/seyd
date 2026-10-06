// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
//! Self-signed certificates for `serverCertificateHashes`.
//!
//! Chrome accepts a self-signed certificate over WebTransport only if it is
//! ECDSA (P-256 here), valid for at most 14 days, and — for IP-addressed
//! servers — carries the IP in `subjectAltName`. The pilot pins the SHA-256 of
//! the DER bytes, which the signaling cloud forwards from `announce`.

use sha2::{Digest, Sha256};
use std::net::IpAddr;
use time::{Duration, OffsetDateTime};

/// Validity chosen one day inside Chrome's 14-day limit.
pub const VALIDITY_DAYS: i64 = 13;

#[derive(Clone)]
pub struct Cert {
    pub cert_der: Vec<u8>,
    /// PKCS#8 private key, DER.
    pub key_der: Vec<u8>,
    pub fingerprint_sha256: [u8; 32],
    pub not_after: OffsetDateTime,
}

impl std::fmt::Debug for Cert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cert")
            .field("fingerprint", &self.fingerprint_hex())
            .field("not_after", &self.not_after)
            .finish()
    }
}

impl Cert {
    /// Generate an ECDSA P-256 certificate with every `ips` entry as a SAN.
    pub fn generate(ips: &[IpAddr]) -> Result<Cert, rcgen::Error> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
        params.subject_alt_names = ips
            .iter()
            .map(|ip| rcgen::SanType::IpAddress(*ip))
            .collect();
        let now = OffsetDateTime::now_utc();
        // A minute of skew tolerance: a pilot clock slightly ahead of the robot
        // must not reject a certificate that was just minted.
        params.not_before = now - Duration::minutes(1);
        params.not_after = now + Duration::days(VALIDITY_DAYS);
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "seyd-agent");
        params.distinguished_name = dn;
        let not_after = params.not_after;
        let cert = params.self_signed(&key)?;
        let cert_der = cert.der().to_vec();
        let fingerprint_sha256: [u8; 32] = Sha256::digest(&cert_der).into();
        Ok(Cert {
            cert_der,
            key_der: key.serialize_der(),
            fingerprint_sha256,
            not_after,
        })
    }

    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.fingerprint_sha256)
    }

    /// Days until expiry; rotation is due at day 10 of 13.
    pub fn days_left(&self) -> f64 {
        (self.not_after - OffsetDateTime::now_utc()).as_seconds_f64() / 86_400.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_short_lived_ecdsa_cert() {
        let c = Cert::generate(&["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()]).unwrap();
        assert_eq!(c.fingerprint_hex().len(), 64);
        assert!(c.days_left() > 12.9 && c.days_left() <= 13.0);
        assert!(!c.key_der.is_empty());
    }
}
