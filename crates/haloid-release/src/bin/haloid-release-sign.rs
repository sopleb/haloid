//! haloid-release-sign — write the signed manifest for one built binary.
//!
//! ```text
//! haloid-release-sign --file <binary> --version 0.2.0 --edition community \
//!     --target x86_64-unknown-linux-musl --url <download url> [--notes-url <url>]
//! haloid-release-sign --image <repository>@sha256:<digest> --version 0.2.0 \
//!     --edition community [--notes-url <url>]
//! ```
//!
//! The first describes a binary, for binary installs; the second an image,
//! for Docker installs and their updater sidecar.
//!
//! The signing seed (64 hex characters) comes from `HALOID_RELEASE_SIGNING_KEY`,
//! or from the file `HALOID_RELEASE_SIGNING_KEY_FILE` names. It is never an
//! argument, so it stays out of process listings and shell history. The
//! manifest goes to standard output.

use haloid_release::{Edition, ImageRelease, Manifest, RELEASE_KEYS, public_key, sign, sign_image};
use sha2::{Digest, Sha256};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(manifest) => {
            println!("{manifest}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("haloid-release-sign: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    let (mut file, mut version, mut edition, mut target, mut url, mut notes_url, mut image) =
        (None, None, None, None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--file" => file = Some(value),
            "--version" => version = Some(value.trim_start_matches('v').to_string()),
            "--edition" => edition = Some(value),
            "--target" => target = Some(value),
            "--url" => url = Some(value),
            "--notes-url" => notes_url = Some(value),
            "--image" => image = Some(value),
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    let need = |v: Option<String>, name: &str| v.ok_or(format!("--{name} is required"));
    let edition = match need(edition, "edition")?.as_str() {
        "community" => Edition::Community,
        "pro" => Edition::Pro,
        other => return Err(format!("{other:?} is not community or pro")),
    };
    let seed = match std::env::var("HALOID_RELEASE_SIGNING_KEY") {
        Ok(seed) => seed,
        Err(_) => {
            let path = std::env::var("HALOID_RELEASE_SIGNING_KEY_FILE").map_err(|_| {
                "set HALOID_RELEASE_SIGNING_KEY or HALOID_RELEASE_SIGNING_KEY_FILE".to_string()
            })?;
            std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?
        }
    };
    let seed: [u8; 32] = hex::decode(seed.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("the signing key is not 32 bytes of hex")?;
    if !RELEASE_KEYS.contains(&public_key(&seed)) {
        eprintln!(
            "haloid-release-sign: warning: this key is not in RELEASE_KEYS; builds will refuse it"
        );
    }
    if let Some(image) = image {
        let release = ImageRelease {
            version: need(version, "version")?,
            edition,
            image,
            notes_url,
            published_at: chrono::Utc::now(),
        };
        let text = sign_image(&release, &seed);
        haloid_release::verify_image(&text, &[public_key(&seed)]).map_err(|e| e.to_string())?;
        return Ok(text);
    }
    let file = need(file, "file")?;
    let bytes = std::fs::read(&file).map_err(|e| format!("reading {file}: {e}"))?;
    let manifest = Manifest {
        version: need(version, "version")?,
        edition,
        target: need(target, "target")?,
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
        url: need(url, "url")?,
        notes_url,
        published_at: chrono::Utc::now(),
    };
    let text = sign(&manifest, &seed);
    // What the server will do with it, so a bad manifest never ships.
    haloid_release::verify(&text, &[public_key(&seed)]).map_err(|e| e.to_string())?;
    Ok(text)
}
