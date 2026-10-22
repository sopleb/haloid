//! Release manifests: signing, checking, and deciding whether one applies.

use chrono::{TimeZone, Utc};
use haloid_release::{Edition, Expect, Manifest, ReleaseError, public_key, sign, verify};

const SIGNER: [u8; 32] = [9; 32];
const OTHER: [u8; 32] = [3; 32];

fn manifest(version: &str) -> Manifest {
    Manifest {
        version: version.into(),
        edition: Edition::Community,
        target: "x86_64-unknown-linux-musl".into(),
        size: 41_234_567,
        sha256: "ab".repeat(32),
        url: "https://git.plb.so/sopleb/haloid/-/releases/v0.2.0/downloads/haloid-server".into(),
        notes_url: Some("https://git.plb.so/sopleb/haloid/-/releases/v0.2.0".into()),
        published_at: Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    }
}

fn expect(current: &str) -> Expect<'_> {
    Expect {
        edition: Edition::Community,
        target: "x86_64-unknown-linux-musl",
        current,
    }
}

#[test]
fn a_signed_manifest_reads_back_as_written() {
    let text = sign(&manifest("0.2.0"), &SIGNER);
    assert!(text.starts_with("HALOIDREL1."));
    assert_eq!(
        verify(&text, &[public_key(&SIGNER)]).unwrap(),
        manifest("0.2.0")
    );
}

#[test]
fn a_manifest_signed_by_anyone_else_or_altered_is_refused() {
    let text = sign(&manifest("0.2.0"), &OTHER);
    assert_eq!(
        verify(&text, &[public_key(&SIGNER)]),
        Err(ReleaseError::BadSignature)
    );

    // The same signature over a payload pointing somewhere else.
    let good = sign(&manifest("0.2.0"), &SIGNER);
    let mut forged = manifest("0.2.0");
    forged.url = "https://evil.example.com/haloid-server".into();
    let forged_payload = sign(&forged, &OTHER).split('.').nth(1).unwrap().to_string();
    let parts: Vec<&str> = good.split('.').collect();
    let spliced = format!("{}.{}.{}", parts[0], forged_payload, parts[2]);
    assert_eq!(
        verify(&spliced, &[public_key(&SIGNER)]),
        Err(ReleaseError::BadSignature)
    );

    assert_eq!(
        verify("HALOIDREL1.nonsense", &[public_key(&SIGNER)]),
        Err(ReleaseError::Malformed)
    );
    // A licence key is not a release.
    assert_eq!(
        verify("HALOID1.e30.e30", &[public_key(&SIGNER)]),
        Err(ReleaseError::Malformed)
    );
}

#[test]
fn only_a_newer_build_of_this_edition_and_target_applies() {
    let release = manifest("0.2.0");
    assert_eq!(release.applies(&expect("0.1.0")), Ok(()));
    assert_eq!(release.applies(&expect("0.1.9")), Ok(()));
    assert_eq!(
        release.applies(&expect("0.2.0")),
        Err(ReleaseError::NotNewer)
    );
    assert_eq!(
        release.applies(&expect("0.10.0")),
        Err(ReleaseError::NotNewer)
    );
    // A pre-release of the same version is older than the release.
    assert_eq!(release.applies(&expect("0.2.0-rc.1")), Ok(()));

    let pro = Expect {
        edition: Edition::Pro,
        ..expect("0.1.0")
    };
    assert_eq!(release.applies(&pro), Err(ReleaseError::OtherEdition));
    let arm = Expect {
        target: "aarch64-unknown-linux-musl",
        ..expect("0.1.0")
    };
    assert_eq!(release.applies(&arm), Err(ReleaseError::OtherTarget));

    let mut odd = manifest("two");
    odd.version = "two".into();
    assert_eq!(odd.applies(&expect("0.1.0")), Err(ReleaseError::Malformed));
}

#[test]
fn a_manifest_must_name_an_https_download_and_a_real_hash() {
    let refused = |m: Manifest| verify(&sign(&m, &SIGNER), &[public_key(&SIGNER)]);
    let mut plain = manifest("0.2.0");
    plain.url = "http://git.plb.so/haloid-server".into();
    assert_eq!(refused(plain), Err(ReleaseError::Malformed));
    // Plain HTTP only to this machine, for tests.
    let mut local = manifest("0.2.0");
    local.url = "http://127.0.0.1:9/haloid-server".into();
    assert!(refused(local).is_ok());
    let mut short = manifest("0.2.0");
    short.sha256 = "abc".into();
    assert_eq!(refused(short), Err(ReleaseError::Malformed));
}

#[test]
fn this_build_knows_its_target() {
    assert!(haloid_release::BUILD_TARGET.contains('-'));
}
