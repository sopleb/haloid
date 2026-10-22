//! haloid-id — public identifier generation and base62 coding.
//!
//! Public ids are **random**, not an encoding of the row id. Encoding the
//! primary key with an alphabet shuffled from an install secret adds length
//! but no entropy, and leaves you with a salt you can never lose. We store a random id in an indexed column instead:
//! decoding is a lookup we were already doing, and there is nothing to leak.
//!
//! The arithmetic [`encode`] / [`decode`] pair is kept for ids where
//! guessability does not matter and a round-trip is convenient.

use rand::Rng;

/// Standard base62, in ASCII order. Not shuffled — see the module docs.
pub const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

const BASE: u64 = 62;

/// Length of a generated public id. 62^10 is ~59 bits.
pub const PUBLIC_ID_LEN: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("empty identifier")]
    Empty,
    #[error("identifier contains an invalid character: {0:?}")]
    InvalidChar(char),
    #[error("identifier is too large to decode")]
    Overflow,
}

/// Encode an integer as base62. `0` encodes to `"0"`, not the empty string.
pub fn encode(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    // u64::MAX is 11 base62 digits.
    let mut buf = [0u8; 11];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = ALPHABET[(n % BASE) as usize];
        n /= BASE;
    }
    // Every byte came from ALPHABET, which is ASCII.
    String::from_utf8(buf[i..].to_vec()).expect("alphabet is ASCII")
}

/// Decode a base62 string. Case-sensitive; rejects anything outside
/// [`ALPHABET`] and anything that would not fit in a `u64`.
pub fn decode(s: &str) -> Result<u64, IdError> {
    if s.is_empty() {
        return Err(IdError::Empty);
    }
    let mut n: u64 = 0;
    for ch in s.chars() {
        let digit = digit_of(ch).ok_or(IdError::InvalidChar(ch))?;
        n = n
            .checked_mul(BASE)
            .and_then(|n| n.checked_add(digit))
            .ok_or(IdError::Overflow)?;
    }
    Ok(n)
}

fn digit_of(ch: char) -> Option<u64> {
    let value = match ch {
        '0'..='9' => ch as u64 - '0' as u64,
        'A'..='Z' => ch as u64 - 'A' as u64 + 10,
        'a'..='z' => ch as u64 - 'a' as u64 + 36,
        _ => return None,
    };
    Some(value)
}

/// A fresh random public id, [`PUBLIC_ID_LEN`] characters of base62.
pub fn generate_public_id() -> String {
    generate_public_id_with(&mut rand::rng())
}

/// [`generate_public_id`] over a caller-supplied RNG, for deterministic tests.
///
/// Uses a uniform range per character rather than folding random bytes with
/// `%`, which would bias the first 4 characters of the alphabet (256 % 62).
pub fn generate_public_id_with<R: Rng + ?Sized>(rng: &mut R) -> String {
    (0..PUBLIC_ID_LEN)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}
