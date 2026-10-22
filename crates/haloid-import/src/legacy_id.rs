//! Chevereto's public ids, so imported files, albums and users keep the
//! ids their old links use.
//!
//! Chevereto encodes a row id in base 62 over an alphabet shuffled by its
//! `crypt_salt`: the alphabet's characters are paired with the first 62
//! hex digits of `sha256(salt)` and sorted by those digits, descending, ties
//! broken by the character, ascending. `id_padding`, when set, multiplies
//! the id first.
//!
//! The digit count comes from a floating-point `floor(log(n, 62))`, as in
//! the original; it is reproduced as written rather than corrected, because
//! the point is to produce the ids Chevereto produced.

use sha2::{Digest, Sha256, Sha512};

const INDEX: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

#[derive(Debug, Clone)]
pub struct LegacyIds {
    alphabet: [u8; 62],
    padding: u64,
}

impl LegacyIds {
    pub fn new(salt: &str, padding: u64) -> Self {
        let mut hash = hex::encode(Sha256::digest(salt.as_bytes()));
        if hash.len() < INDEX.len() {
            hash = hex::encode(Sha512::digest(salt.as_bytes()));
        }
        let mut pairs: Vec<(u8, u8)> = hash
            .bytes()
            .zip(INDEX.iter().copied())
            .take(INDEX.len())
            .collect();
        // array_multisort($p, SORT_DESC, $i): by digit descending, then by
        // character ascending.
        pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut alphabet = [0u8; 62];
        for (slot, (_, c)) in alphabet.iter_mut().zip(pairs) {
            *slot = c;
        }
        Self { alphabet, padding }
    }

    /// The alphabet, for a test to compare with Chevereto's.
    pub fn alphabet(&self) -> String {
        String::from_utf8_lossy(&self.alphabet).into_owned()
    }

    /// The public id Chevereto gave row `id`.
    pub fn encode(&self, id: u64) -> String {
        let mut n = if self.padding > 0 {
            id * self.padding
        } else {
            id
        };
        if n == 0 {
            return String::new();
        }
        let base = 62u64;
        let top = ((n as f64).ln() / (base as f64).ln()).floor() as i32;
        let mut out = String::new();
        for t in (0..=top).rev() {
            let power = base.pow(t as u32);
            let digit = (n / power) % base;
            out.push(self.alphabet[digit as usize] as char);
            n -= digit * power;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A salt, a padding, the alphabet they give, and ids with their codes.
    type Vector<'a> = (&'a str, u64, &'a str, &'a [(u64, &'a str)]);

    /// Produced by Chevereto's own `cheveretoID()` under PHP 8.
    #[test]
    fn ids_match_what_chevereto_produced() {
        let cases: [Vector<'_>; 3] = [
            (
                "",
                0,
                "1ksu9CaLelnw2HTYct8Mr3GIOQRiqyzVZjx7U0EPW5ADFKNfgpvJb46XhBSmod",
                &[
                    (1, "k"),
                    (2, "s"),
                    (61, "d"),
                    (62, "k1"),
                    (63, "kk"),
                    (3843, "dd"),
                    (3844, "k11"),
                    (3845, "k1k"),
                    (238327, "ddd"),
                    (238328, "k111"),
                    (238329, "k11k"),
                    (1000000, "92ls"),
                    (14776336, "k1111"),
                    (2147483647, "s3rE0k"),
                ],
            ),
            (
                "salty",
                0,
                "NWv2BCQk5GLTUq39HVZo1DrPlnsuw4AOmxXKYde7Ei6jfyISc8JbghMRpz0Fat",
                &[
                    (1, "W"),
                    (62, "WN"),
                    (3844, "WNN"),
                    (238328, "WNNN"),
                    (1000000, "BUGv"),
                    (2147483647, "vD1edW"),
                ],
            ),
            (
                "b1Ux3",
                5000,
                "46BIMNcip3UryxFb0mns8GKPjOl57HREXkwzTVZahotvQSqC1DdfAJLWYu29eg",
                &[
                    (1, "6nh"),
                    (2, "BVn"),
                    (61, "6mGK"),
                    (62, "6nh4"),
                    (3844, "6nh44"),
                    (238329, "6nh6nh"),
                    (1000000, "N7PROn"),
                    (2147483647, "IIBPcoUh"),
                ],
            ),
        ];
        for (salt, padding, alphabet, ids) in cases {
            let legacy = LegacyIds::new(salt, padding);
            assert_eq!(legacy.alphabet(), alphabet, "salt {salt:?}");
            for (id, expected) in ids {
                assert_eq!(legacy.encode(*id), *expected, "salt {salt:?}, id {id}");
            }
        }
    }
}
