//! Passwords, session tokens and API keys: making them and checking them.
//!
//! Nothing secret is ever stored as given. Passwords are argon2id PHC
//! strings; session tokens are stored as their sha256; API key secrets as an
//! HMAC-SHA256 under a per-key HMAC key. Every comparison of a secret is
//! constant-time.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, password_hash::rand_core::OsRng};
use haloid_core::DomainError;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use subtle::ConstantTimeEq;

fn internal(reason: impl std::fmt::Display) -> DomainError {
    DomainError::Internal(reason.to_string().into())
}

/// Hash a password as an argon2id PHC string, with the crate's defaults
/// (19 MiB, two passes, one lane — the OWASP baseline). Slow on purpose:
/// call it from a blocking thread.
pub fn hash_password(password: &str) -> Result<String, DomainError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(internal)
}

/// Whether `password` matches the stored hash. A malformed stored hash is
/// a mismatch, not an error: it can only mean the row is wrong.
///
/// Haloid writes argon2id PHC strings. bcrypt (`$2y$…`, as PHP's
/// `password_hash` writes it) is also read, for accounts brought over by the
/// importer; [`needs_rehash`] says to replace one at the next login.
pub fn verify_password(password: &str, stored: &str) -> bool {
    if stored.starts_with("$2") {
        return bcrypt::verify(password, stored).unwrap_or(false);
    }
    PasswordHash::new(stored).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

/// Whether a stored hash is in a form Haloid only reads, and should be
/// replaced with an argon2id one now the password is known.
pub fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

/// A real hash of nothing in particular, verified against when the login
/// names no account, so that "no such user" takes as long as "wrong
/// password" and timing does not reveal which accounts exist.
static DECOY: LazyLock<String> =
    LazyLock::new(|| hash_password("decoy password, never matches").expect("argon2 works"));

pub fn verify_against_decoy(password: &str) {
    let _ = verify_password(password, &DECOY);
}

/// `len` characters of base62 from the OS RNG.
fn random_base62(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| haloid_id::ALPHABET[rng.random_range(0..haloid_id::ALPHABET.len())] as char)
        .collect()
}

/// Lowercase hex sha256, the stored form of a session token.
pub fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// A new session token: 43 base62 characters, ~256 bits. The cookie holds
/// it; the database holds [`sha256_hex`] of it.
pub fn new_session_token() -> String {
    random_base62(43)
}

/// A freshly minted API key. `key` is shown to its owner once and never
/// stored; the rest is what the database keeps.
#[derive(Debug, Clone)]
pub struct MintedKey {
    pub key: String,
    pub prefix: String,
    pub secret_hash: String,
    pub hmac_key: [u8; 32],
}

const KEY_TAG: &str = "hld_";
const KEY_ID_LEN: usize = haloid_id::PUBLIC_ID_LEN;
const KEY_SECRET_LEN: usize = 43;

/// Mint `hld_{id}_{secret}`: a random 10-character id, which with the tag
/// and separator is the public lookup prefix, and a 43-character secret.
pub fn mint_api_key() -> MintedKey {
    let prefix = format!("{KEY_TAG}{}_", haloid_id::generate_public_id());
    let secret = random_base62(KEY_SECRET_LEN);
    let mut hmac_key = [0u8; 32];
    rand::rng().fill(&mut hmac_key);
    MintedKey {
        key: format!("{prefix}{secret}"),
        secret_hash: key_hash(&hmac_key, &secret),
        prefix,
        hmac_key,
    }
}

/// Split a presented key into its lookup prefix and secret. `None` for
/// anything not shaped like one of ours, before touching the database.
pub fn split_api_key(key: &str) -> Option<(&str, &str)> {
    let key = key.trim();
    let prefix_len = KEY_TAG.len() + KEY_ID_LEN + 1;
    if key.len() != prefix_len + KEY_SECRET_LEN || !key.is_ascii() || !key.starts_with(KEY_TAG) {
        return None;
    }
    let (prefix, secret) = key.split_at(prefix_len);
    let id = &prefix[KEY_TAG.len()..prefix_len - 1];
    let well_formed = prefix.ends_with('_')
        && id
            .bytes()
            .chain(secret.bytes())
            .all(|b| b.is_ascii_alphanumeric());
    well_formed.then_some((prefix, secret))
}

/// The stored form of an API key secret.
pub fn key_hash(hmac_key: &[u8], secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(hmac_key).expect("HMAC takes any key length");
    mac.update(secret.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time equality of two stored-form hashes.
pub fn hashes_match(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bcrypt_hash_from_php_verifies_and_asks_to_be_replaced() {
        // PHP: password_hash("correct horse battery", PASSWORD_BCRYPT, ["cost" => 4])
        let php = "$2y$04$IpH.kt2ZZ/Qdh3ncMlrk8O3nRU9s5cLCRd9W5OGlxJiZtHqWL/j9i";
        assert!(verify_password("correct horse battery", php));
        assert!(!verify_password("wrong", php));
        assert!(needs_rehash(php));
        assert!(!needs_rehash(&hash_password("x").unwrap()));
    }

    #[test]
    fn passwords_verify_and_mismatch() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("correct horse batterY", &hash));
        assert!(!verify_password("anything", "not a phc string"));
        assert_ne!(
            hash,
            hash_password("correct horse battery").unwrap(),
            "salted: the same password never hashes the same way twice"
        );
    }

    #[test]
    fn session_tokens_are_long_random_and_stored_hashed() {
        let a = new_session_token();
        let b = new_session_token();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert_eq!(sha256_hex(&a).len(), 64);
        assert_ne!(sha256_hex(&a), a);
    }

    #[test]
    fn minted_keys_split_back_and_verify() {
        let minted = mint_api_key();
        assert!(minted.key.starts_with("hld_"), "{}", minted.key);
        assert_eq!(minted.prefix.len(), 15);
        assert!(minted.prefix.len() <= 48, "fits the column");

        let (prefix, secret) = split_api_key(&minted.key).expect("our own key parses");
        assert_eq!(prefix, minted.prefix);
        assert!(hashes_match(
            &key_hash(&minted.hmac_key, secret),
            &minted.secret_hash
        ));
        assert!(!hashes_match(
            &key_hash(&minted.hmac_key, "wrong"),
            &minted.secret_hash
        ));
        assert!(
            split_api_key(&format!("  {}\n", minted.key)).is_some(),
            "pasted whitespace is forgiven"
        );
    }

    #[test]
    fn foreign_and_malformed_keys_never_reach_the_database() {
        let minted = mint_api_key();
        let wrong_tag = minted.key.replacen("hld_", "abc_", 1);
        let short = &minted.key[..minted.key.len() - 1];
        let punctuated = format!("{}!", &minted.key[..minted.key.len() - 1]);
        for key in [
            "",
            "hld_",
            short,
            wrong_tag.as_str(),
            punctuated.as_str(),
            "xyz_aaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ] {
            assert!(split_api_key(key).is_none(), "{key:?} accepted");
        }
    }
}
