//! Human verification with ALTCHA proof-of-work, protocol v2.
//!
//! The official `altcha` crate does the cryptography: challenge creation,
//! signing, and verification including the `keyPrefix` check whose absence
//! would make the proof-of-work free. We use Deterministic Effort Mode —
//! the server picks the counter, so the solution's derived key can be
//! checked by HMAC alone instead of a key derivation per verification.
//!
//! Replay protection is ours, because a custom integration owns it: each
//! challenge's nonce may be redeemed once. The store's test-and-set is
//! atomic — one lock held across the check and the insert — so two
//! concurrent submissions of the same payload cannot both pass. A nonce is
//! recorded only after the payload verifies, so garbage cannot fill the
//! store or burn somebody else's challenge.
//!
//! This is proof-of-work, not bot detection: a cost multiplier on bulk
//! automation. It sits beside rate limiting and account lockout, never in
//! place of them.

use altcha::{CreateChallengeOptions, Payload, VerifySolutionOptions};
use base64::Engine;
use haloid_core::DomainError;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AltchaConfig {
    /// Signs challenges. Instance-local and never stored in the database.
    pub secret: String,
    /// PBKDF2 iterations per attempt.
    pub cost: u32,
    /// The server-chosen counter is drawn from this range; the client has to
    /// try about that many derivations to find it.
    pub counter_min: u32,
    pub counter_max: u32,
    pub lifetime: Duration,
    /// Most outstanding redeemed nonces remembered at once.
    pub replay_capacity: usize,
}

impl AltchaConfig {
    /// The production parameters from the design: PBKDF2/SHA-256, cost 5000,
    /// counter 5000 to 10000, ten-minute lifetime. Kept modest on purpose:
    /// there is no alternative path for a device that cannot finish it.
    pub fn with_secret(secret: String) -> Self {
        Self {
            secret,
            cost: 5000,
            counter_min: 5000,
            counter_max: 10_000,
            lifetime: Duration::from_secs(600),
            replay_capacity: 100_000,
        }
    }
}

pub struct Altcha {
    config: AltchaConfig,
    /// Redeemed nonce → the challenge's expiry (unix seconds). An entry is
    /// worthless once its challenge has expired, since an expired challenge
    /// fails verification anyway.
    redeemed: Mutex<HashMap<String, i64>>,
}

fn rejected() -> DomainError {
    DomainError::HumanVerificationFailed
}

impl Altcha {
    pub fn new(config: AltchaConfig) -> Self {
        Self {
            config,
            redeemed: Mutex::new(HashMap::new()),
        }
    }

    /// The key-signature secret is derived from the main one rather than
    /// configured separately: one secret for an operator to manage.
    fn key_secret(&self) -> String {
        format!("{}:key-signature", self.config.secret)
    }

    /// A fresh signed challenge, as the JSON the widget expects.
    pub fn challenge(&self, now_unix: i64) -> Result<serde_json::Value, DomainError> {
        let counter = rand::rng().random_range(self.config.counter_min..=self.config.counter_max);
        let challenge = altcha::create_challenge(CreateChallengeOptions {
            algorithm: "PBKDF2/SHA-256".into(),
            counter: Some(counter),
            cost: self.config.cost,
            expires_at: Some(now_unix + self.config.lifetime.as_secs() as i64),
            hmac_signature_secret: Some(self.config.secret.clone()),
            hmac_key_signature_secret: Some(self.key_secret()),
            ..Default::default()
        })
        .map_err(|e| DomainError::Internal(e.to_string().into()))?;
        serde_json::to_value(challenge).map_err(|e| DomainError::Internal(e.into()))
    }

    /// Check the widget's form field — base64 of the JSON payload — and
    /// redeem its challenge. Any failure is the same error.
    pub fn verify(&self, field: &str, now_unix: i64) -> Result<(), DomainError> {
        let json = base64::engine::general_purpose::STANDARD
            .decode(field.trim())
            .map_err(|_| rejected())?;
        let payload: Payload = serde_json::from_slice(&json).map_err(|_| rejected())?;

        // Our challenges always expire; one that does not was not ours, and
        // could never be forgotten by the replay store.
        let Some(expires_at) = payload.challenge.parameters.expires_at.filter(|&t| t > 0) else {
            return Err(rejected());
        };

        let mut options =
            VerifySolutionOptions::new(&payload.challenge, &payload.solution, &self.config.secret);
        options.hmac_key_signature_secret = Some(self.key_secret());
        let result = altcha::verify_solution(options).map_err(|_| rejected())?;
        if !result.verified {
            return Err(rejected());
        }

        self.redeem(&payload.challenge.parameters.nonce, expires_at, now_unix)
    }

    /// Atomic test-and-set: true exactly once per nonce.
    fn redeem(&self, nonce: &str, expires_at: i64, now_unix: i64) -> Result<(), DomainError> {
        let mut redeemed = self
            .redeemed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if redeemed.contains_key(nonce) {
            return Err(rejected());
        }
        if redeemed.len() >= self.config.replay_capacity {
            redeemed.retain(|_, &mut expiry| expiry >= now_unix);
            // Still full of live entries: fail closed rather than forget a
            // nonce that could then be replayed.
            if redeemed.len() >= self.config.replay_capacity {
                return Err(rejected());
            }
        }
        redeemed.insert(nonce.to_string(), expires_at);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use altcha::{Challenge, SolveChallengeOptions};

    /// Cheap parameters, so tests solve in milliseconds.
    pub(crate) fn test_config() -> AltchaConfig {
        AltchaConfig {
            cost: 1,
            counter_min: 1,
            counter_max: 20,
            ..AltchaConfig::with_secret("test-secret".into())
        }
    }

    /// Do what the browser widget does: solve, then base64 the payload.
    pub(crate) fn solve(challenge: &serde_json::Value) -> String {
        let challenge: Challenge = serde_json::from_value(challenge.clone()).unwrap();
        let solution = altcha::solve_challenge(SolveChallengeOptions::new(&challenge))
            .unwrap()
            .expect("solvable");
        let payload = Payload {
            challenge,
            solution,
        };
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap())
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn a_solved_challenge_verifies_once_and_only_once() {
        let altcha = Altcha::new(test_config());
        let field = solve(&altcha.challenge(now()).unwrap());
        altcha.verify(&field, now()).expect("first redemption");
        assert!(
            matches!(
                altcha.verify(&field, now()),
                Err(DomainError::HumanVerificationFailed)
            ),
            "a replayed payload was accepted"
        );
    }

    #[test]
    fn concurrent_replays_cannot_both_pass() {
        let altcha = std::sync::Arc::new(Altcha::new(test_config()));
        let field = solve(&altcha.challenge(now()).unwrap());
        let passes: usize = (0..16)
            .map(|_| {
                let altcha = altcha.clone();
                let field = field.clone();
                std::thread::spawn(move || altcha.verify(&field, now()).is_ok())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum();
        assert_eq!(passes, 1, "{passes} of 16 concurrent submissions passed");
    }

    #[test]
    fn another_instances_challenge_is_rejected() {
        let ours = Altcha::new(test_config());
        let theirs = Altcha::new(AltchaConfig {
            secret: "someone else".into(),
            ..test_config()
        });
        let field = solve(&theirs.challenge(now()).unwrap());
        assert!(ours.verify(&field, now()).is_err());
    }

    #[test]
    fn an_expired_challenge_is_rejected() {
        let altcha = Altcha::new(test_config());
        let field = solve(&altcha.challenge(now() - 3600).unwrap());
        assert!(altcha.verify(&field, now()).is_err());
    }

    #[test]
    fn a_wrong_solution_is_rejected_without_burning_the_challenge() {
        let altcha = Altcha::new(test_config());
        let challenge = altcha.challenge(now()).unwrap();
        let good = solve(&challenge);

        let mut payload: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(&good)
                .unwrap(),
        )
        .unwrap();
        payload["solution"]["derivedKey"] = "00".repeat(32).into();
        let forged =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload).unwrap());

        assert!(altcha.verify(&forged, now()).is_err());
        altcha
            .verify(&good, now())
            .expect("the failed attempt did not consume the nonce");
    }

    #[test]
    fn garbage_is_rejected() {
        let altcha = Altcha::new(test_config());
        for field in ["", "not base64!", "e30=", "bnVsbA=="] {
            assert!(altcha.verify(field, now()).is_err(), "{field:?}");
        }
    }

    #[test]
    fn a_full_store_fails_closed() {
        let altcha = Altcha::new(AltchaConfig {
            replay_capacity: 2,
            ..test_config()
        });
        for _ in 0..2 {
            let field = solve(&altcha.challenge(now()).unwrap());
            altcha.verify(&field, now()).unwrap();
        }
        let field = solve(&altcha.challenge(now()).unwrap());
        assert!(
            altcha.verify(&field, now()).is_err(),
            "the store forgot a live nonce to make room"
        );
        // Once those challenges have expired their entries can go, and there
        // is room again.
        assert!(
            altcha
                .redeem("fresh-nonce", now() + 7200, now() + 3600)
                .is_ok()
        );
    }
}
