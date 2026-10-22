//! Haloid release manifests: what a build is, where to get it, and proof
//! that it came from us.
//!
//! A manifest is written `HALOIDREL1.<payload>.<signature>`, the licence
//! key's shape: the payload is the [`Manifest`] as JSON, base64url without
//! padding, and the signature is Ed25519 over the payload's bytes. It is
//! signed with a release key, never a licence key, so a leaked licence seed
//! cannot ship code. The updater trusts [`RELEASE_KEYS`] and nothing else,
//! then checks the download against the manifest's size and SHA-256.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "HALOIDREL1";

/// The public halves of the keys releases are signed with. The seeds are
/// kept off every repository; see `docs/deploy.md`.
pub const RELEASE_KEYS: &[[u8; 32]] = &[[
    0x79, 0xa8, 0x49, 0x03, 0x15, 0x5b, 0xb8, 0xea, 0xa5, 0x6f, 0xe6, 0x72, 0x6d, 0x1e, 0x71, 0xdf,
    0x9a, 0x08, 0xf0, 0xce, 0x65, 0xe1, 0x31, 0x28, 0x52, 0x55, 0x8f, 0xfa, 0x20, 0xee, 0xad, 0x65,
]];

/// The target triple this binary was built for.
pub const BUILD_TARGET: &str = env!("HALOID_BUILD_TARGET");

/// Which build: the public one, or the one with the Pro crates linked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    Community,
    Pro,
}

impl Edition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Community => "community",
            Self::Pro => "pro",
        }
    }
}

/// One build of one release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Semantic version, without a leading `v`.
    pub version: String,
    pub edition: Edition,
    /// The target triple, e.g. `x86_64-unknown-linux-musl`.
    pub target: String,
    /// The binary's length in bytes.
    pub size: u64,
    /// The binary's SHA-256, lower-case hex.
    pub sha256: String,
    /// Where to download the binary: HTTPS, or plain HTTP to this machine.
    pub url: String,
    /// The release notes, for the admin page.
    pub notes_url: Option<String>,
    pub published_at: DateTime<Utc>,
}

/// What the running binary is, for [`Manifest::applies`].
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    pub edition: Edition,
    pub target: &'a str,
    /// The running version.
    pub current: &'a str,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReleaseError {
    #[error("this is not a Haloid release manifest")]
    Malformed,
    #[error("the release's signature does not check out")]
    BadSignature,
    #[error("the release is for the other edition")]
    OtherEdition,
    #[error("the release is for another platform")]
    OtherTarget,
    #[error("the release is not newer than this build")]
    NotNewer,
}

impl Manifest {
    /// Is this a build the running binary should move to?
    pub fn applies(&self, expect: &Expect<'_>) -> Result<(), ReleaseError> {
        if self.edition != expect.edition {
            return Err(ReleaseError::OtherEdition);
        }
        if self.target != expect.target {
            return Err(ReleaseError::OtherTarget);
        }
        let offered = semver::Version::parse(&self.version).map_err(|_| ReleaseError::Malformed)?;
        let current =
            semver::Version::parse(expect.current).map_err(|_| ReleaseError::Malformed)?;
        if offered <= current {
            return Err(ReleaseError::NotNewer);
        }
        Ok(())
    }

    fn well_formed(&self) -> bool {
        let https = self.url.starts_with("https://");
        let loopback = [
            "http://127.0.0.1:",
            "http://127.0.0.1/",
            "http://localhost:",
            "http://localhost/",
        ]
        .iter()
        .any(|p| self.url.starts_with(p));
        (https || loopback)
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && self.size > 0
    }
}

/// Check a `<prefix>.<payload>.<signature>` text and return the payload.
fn open(text: &str, prefix: &str, trusted: &[[u8; 32]]) -> Result<Vec<u8>, ReleaseError> {
    let mut parts = text.trim().split('.');
    let (Some(found), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ReleaseError::Malformed);
    };
    if found != prefix {
        return Err(ReleaseError::Malformed);
    }
    let payload = B64.decode(payload).map_err(|_| ReleaseError::Malformed)?;
    let signature: [u8; 64] = B64
        .decode(signature)
        .map_err(|_| ReleaseError::Malformed)?
        .try_into()
        .map_err(|_| ReleaseError::Malformed)?;
    let signature = Signature::from_bytes(&signature);
    let signed = trusted.iter().any(|key| {
        VerifyingKey::from_bytes(key)
            .is_ok_and(|key| key.verify_strict(&payload, &signature).is_ok())
    });
    if !signed {
        return Err(ReleaseError::BadSignature);
    }
    Ok(payload)
}

#[cfg(feature = "sign")]
fn seal(prefix: &str, payload: &[u8], signing_key: &[u8; 32]) -> String {
    use ed25519_dalek::{Signer, SigningKey};
    let signature = SigningKey::from_bytes(signing_key).sign(payload);
    format!(
        "{prefix}.{}.{}",
        B64.encode(payload),
        B64.encode(signature.to_bytes())
    )
}

/// Read a manifest and check its signature against `trusted`.
pub fn verify(text: &str, trusted: &[[u8; 32]]) -> Result<Manifest, ReleaseError> {
    let payload = open(text, PREFIX, trusted)?;
    let manifest: Manifest =
        serde_json::from_slice(&payload).map_err(|_| ReleaseError::Malformed)?;
    if !manifest.well_formed() {
        return Err(ReleaseError::Malformed);
    }
    Ok(manifest)
}

/// Write a manifest: for the release pipeline, and for tests.
#[cfg(feature = "sign")]
pub fn sign(manifest: &Manifest, signing_key: &[u8; 32]) -> String {
    let payload = serde_json::to_vec(manifest).expect("a manifest always serialises");
    seal(PREFIX, &payload, signing_key)
}

/// The prefix of an image release.
pub const IMAGE_PREFIX: &str = "HALOIDIMG1";

/// One release as a container image, for Docker installs: what the updater
/// sidecar pulls. Signed the same way as a [`Manifest`], under its own
/// prefix so neither can stand in for the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRelease {
    pub version: String,
    pub edition: Edition,
    /// `<repository>@sha256:<digest>`: by digest, never by tag, since a tag
    /// can be moved after the release is signed.
    pub image: String,
    pub notes_url: Option<String>,
    pub published_at: DateTime<Utc>,
}

impl ImageRelease {
    /// Is this newer than `current`, and of this edition?
    pub fn applies(&self, edition: Edition, current: &str) -> Result<(), ReleaseError> {
        if self.edition != edition {
            return Err(ReleaseError::OtherEdition);
        }
        let offered = semver::Version::parse(&self.version).map_err(|_| ReleaseError::Malformed)?;
        let current = semver::Version::parse(current).map_err(|_| ReleaseError::Malformed)?;
        if offered <= current {
            return Err(ReleaseError::NotNewer);
        }
        Ok(())
    }

    /// The image without its digest.
    pub fn repository(&self) -> &str {
        self.image.split('@').next().unwrap_or(&self.image)
    }

    fn well_formed(&self) -> bool {
        let Some((repository, digest)) = self.image.split_once("@sha256:") else {
            return false;
        };
        !repository.is_empty()
            && !repository.contains(char::is_whitespace)
            && digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && semver::Version::parse(&self.version).is_ok()
    }
}

/// Read an image release and check its signature against `trusted`.
pub fn verify_image(text: &str, trusted: &[[u8; 32]]) -> Result<ImageRelease, ReleaseError> {
    let payload = open(text, IMAGE_PREFIX, trusted)?;
    let release: ImageRelease =
        serde_json::from_slice(&payload).map_err(|_| ReleaseError::Malformed)?;
    if !release.well_formed() {
        return Err(ReleaseError::Malformed);
    }
    Ok(release)
}

/// Write an image release: for the release pipeline, and for tests.
#[cfg(feature = "sign")]
pub fn sign_image(release: &ImageRelease, signing_key: &[u8; 32]) -> String {
    let payload = serde_json::to_vec(release).expect("a release always serialises");
    seal(IMAGE_PREFIX, &payload, signing_key)
}

/// The public key for a signing seed.
pub fn public_key(signing_key: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(signing_key)
        .verifying_key()
        .to_bytes()
}
