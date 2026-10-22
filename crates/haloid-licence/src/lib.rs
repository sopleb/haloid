//! haloid-licence — licence keys, and checking one offline.
//!
//! A key is three dot-separated parts:
//!
//! ```text
//! HALOID1.<payload>.<signature>
//! ```
//!
//! `payload` is the licence as JSON and `signature` an Ed25519 signature
//! over the payload's bytes exactly as sent, both unpadded base64url. The
//! server checks the signature against the public keys compiled into it
//! ([`OFFICIAL_KEYS`]), so no network is needed; the signing key exists
//! only in the keygen's hands and never in any repository.
//!
//! A key names the instance it was issued for. Moving it to another
//! instance means a new key, issued after the old one is deactivated
//! through support.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use haloid_core::entitlements::{Entitlements, LicenceKind};
use serde::{Deserialize, Serialize};

/// Every key starts with this; the number is the format's version.
pub const PREFIX: &str = "HALOID1";

/// The public halves of the keys official licences are signed with. More
/// than one, so a signing key can be rotated without invalidating the keys
/// already issued.
pub const OFFICIAL_KEYS: &[[u8; 32]] = &[
    // The first, made 2026-10-22.
    [
        0x8f, 0x2b, 0x0d, 0xda, 0xc8, 0x61, 0x60, 0x9e, 0x97, 0x03, 0x5c, 0x8a, 0xe9, 0xe4, 0x8a,
        0x33, 0x05, 0xf2, 0x93, 0x45, 0xe9, 0x0e, 0x76, 0x54, 0x0d, 0x0e, 0xcf, 0xaf, 0x77, 0x55,
        0xf3, 0xb1,
    ],
];

/// A licence, as signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Licence {
    /// The key's own id, for support and for revocation.
    pub id: String,
    /// The instance it is for: its id, from the instance's admin area.
    pub instance: String,
    /// Who it was issued to.
    pub licensee: String,
    pub kind: Kind,
    pub issued_at: DateTime<Utc>,
    /// `None` for a perpetual key.
    pub expires_at: Option<DateTime<Utc>>,
}

/// [`LicenceKind`], as it appears in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Trial,
    Standard,
    Perpetual,
}

impl From<Kind> for LicenceKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Trial => Self::Trial,
            Kind::Standard => Self::Standard,
            Kind::Perpetual => Self::Perpetual,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LicenceError {
    #[error("this is not a Haloid licence key")]
    Malformed,
    #[error("this key's signature does not check out")]
    BadSignature,
    #[error("this key is for another instance ({0})")]
    OtherInstance(String),
}

impl Licence {
    /// What this licence allows at `now`.
    pub fn entitlements(&self, now: DateTime<Utc>) -> Entitlements {
        Entitlements::for_licence(self.kind.into(), self.expires_at, now)
    }
}

/// Read a key and check its signature against `trusted`. The instance is
/// not checked here; see [`verify_for`].
pub fn verify(key: &str, trusted: &[[u8; 32]]) -> Result<Licence, LicenceError> {
    let mut parts = key.trim().split('.');
    let (Some(PREFIX), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(LicenceError::Malformed);
    };
    let payload = B64.decode(payload).map_err(|_| LicenceError::Malformed)?;
    let signature: [u8; 64] = B64
        .decode(signature)
        .map_err(|_| LicenceError::Malformed)?
        .try_into()
        .map_err(|_| LicenceError::Malformed)?;
    let signature = Signature::from_bytes(&signature);
    let signed = trusted.iter().any(|key| {
        VerifyingKey::from_bytes(key)
            .is_ok_and(|key| key.verify_strict(&payload, &signature).is_ok())
    });
    if !signed {
        return Err(LicenceError::BadSignature);
    }
    serde_json::from_slice(&payload).map_err(|_| LicenceError::Malformed)
}

/// [`verify`], and check the key is for `instance`.
pub fn verify_for(
    key: &str,
    instance: &str,
    trusted: &[[u8; 32]],
) -> Result<Licence, LicenceError> {
    let licence = verify(key, trusted)?;
    if licence.instance != instance {
        return Err(LicenceError::OtherInstance(licence.instance));
    }
    Ok(licence)
}

/// Write a key: for the keygen, and for tests.
#[cfg(feature = "sign")]
pub fn sign(licence: &Licence, signing_key: &[u8; 32]) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    let payload = serde_json::to_vec(licence).expect("a licence always serialises");
    let signature = SigningKey::from_bytes(signing_key).sign(&payload);
    format!(
        "{PREFIX}.{}.{}",
        B64.encode(&payload),
        B64.encode(signature.to_bytes())
    )
}

/// The public half of a signing key, for [`OFFICIAL_KEYS`] or a test's
/// trusted list.
#[cfg(feature = "sign")]
pub fn public_key(signing_key: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(signing_key)
        .verifying_key()
        .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SIGNER: [u8; 32] = [7; 32];

    fn licence() -> Licence {
        Licence {
            id: "lic_1".into(),
            instance: "inst_abc".into(),
            licensee: "Example Ltd".into(),
            kind: Kind::Standard,
            issued_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            expires_at: Some(Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap()),
        }
    }

    #[test]
    fn a_signed_key_verifies_and_reads_back() {
        let key = sign(&licence(), &SIGNER);
        assert!(key.starts_with("HALOID1."));
        let read = verify_for(&key, "inst_abc", &[public_key(&SIGNER)]).unwrap();
        assert_eq!(read, licence());
    }

    #[test]
    fn surrounding_whitespace_from_a_paste_is_fine() {
        let key = format!("  {}\n", sign(&licence(), &SIGNER));
        assert!(verify(&key, &[public_key(&SIGNER)]).is_ok());
    }

    #[test]
    fn a_key_signed_by_anyone_else_is_refused() {
        let key = sign(&licence(), &[9; 32]);
        assert_eq!(
            verify(&key, &[public_key(&SIGNER)]),
            Err(LicenceError::BadSignature)
        );
        assert_eq!(verify(&key, &[]), Err(LicenceError::BadSignature));
    }

    #[test]
    fn an_edited_payload_breaks_the_signature() {
        let key = sign(&licence(), &SIGNER);
        let mut forged = licence();
        forged.kind = Kind::Perpetual;
        forged.expires_at = None;
        let forged_payload = B64.encode(serde_json::to_vec(&forged).unwrap());
        let parts: Vec<&str> = key.split('.').collect();
        let tampered = format!("{}.{forged_payload}.{}", parts[0], parts[2]);
        assert_eq!(
            verify(&tampered, &[public_key(&SIGNER)]),
            Err(LicenceError::BadSignature)
        );
    }

    #[test]
    fn a_key_for_another_instance_is_refused() {
        let key = sign(&licence(), &SIGNER);
        assert_eq!(
            verify_for(&key, "inst_other", &[public_key(&SIGNER)]),
            Err(LicenceError::OtherInstance("inst_abc".into()))
        );
    }

    #[test]
    fn garbage_is_malformed() {
        for key in [
            "",
            "HALOID1",
            "HALOID1.a.b",
            "HALOID2.a.b",
            "HALOID1.a.b.c",
            "nope",
        ] {
            assert_eq!(
                verify(key, &[public_key(&SIGNER)]),
                Err(LicenceError::Malformed),
                "{key}"
            );
        }
    }

    #[test]
    fn the_payload_is_readable_json_with_lowercase_kinds() {
        let key = sign(&licence(), &SIGNER);
        let payload = B64.decode(key.split('.').nth(1).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(json["kind"], "standard");
        assert_eq!(json["instance"], "inst_abc");
    }
}
