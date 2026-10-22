//! Image releases: what a Docker install updates to.

use chrono::{TimeZone, Utc};
use haloid_release::{
    Edition, ImageRelease, ReleaseError, public_key, sign_image, verify, verify_image,
};

const SIGNER: [u8; 32] = [9; 32];

fn release(version: &str) -> ImageRelease {
    ImageRelease {
        version: version.into(),
        edition: Edition::Community,
        image: format!("git.plb.so:5050/sopleb/haloid@sha256:{}", "c0".repeat(32)),
        notes_url: None,
        published_at: Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    }
}

#[test]
fn an_image_release_is_signed_read_back_and_never_mistaken_for_a_binary_one() {
    let text = sign_image(&release("0.2.0"), &SIGNER);
    assert!(text.starts_with("HALOIDIMG1."));
    let keys = [public_key(&SIGNER)];
    assert_eq!(verify_image(&text, &keys).unwrap(), release("0.2.0"));
    assert_eq!(verify(&text, &keys), Err(ReleaseError::Malformed));
    assert_eq!(
        verify_image(&text, &[public_key(&[1; 32])]),
        Err(ReleaseError::BadSignature)
    );
}

#[test]
fn an_image_is_named_by_digest_and_only_a_newer_one_of_this_edition_applies() {
    let keys = [public_key(&SIGNER)];
    // A tag can be moved after signing; a digest cannot.
    let mut tagged = release("0.2.0");
    tagged.image = "git.plb.so:5050/sopleb/haloid:0.2.0".into();
    assert_eq!(
        verify_image(&sign_image(&tagged, &SIGNER), &keys),
        Err(ReleaseError::Malformed)
    );

    let r = release("0.2.0");
    assert_eq!(r.applies(Edition::Community, "0.1.0"), Ok(()));
    assert_eq!(
        r.applies(Edition::Community, "0.2.0"),
        Err(ReleaseError::NotNewer)
    );
    assert_eq!(
        r.applies(Edition::Pro, "0.1.0"),
        Err(ReleaseError::OtherEdition)
    );
    assert_eq!(r.repository(), "git.plb.so:5050/sopleb/haloid");
}
