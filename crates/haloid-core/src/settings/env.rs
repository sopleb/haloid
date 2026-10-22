//! Environment overrides: the top settings layer.
//!
//! Every setting can be pinned from the environment as `HALOID_` plus its key
//! upper-cased, dots as underscores: `uploads.max_size_user` is
//! `HALOID_UPLOADS_MAX_SIZE_USER`. A pinned setting wins over the database
//! and cannot be changed from the admin UI, which says so rather than
//! accepting a write that would have no effect.
//!
//! Reading the process environment is the caller's job; this module takes a
//! lookup function so it stays pure.

use super::{SettingWarning, Settings};
use std::collections::BTreeMap;

/// The environment variable that overrides `key`.
pub fn env_var(key: &str) -> String {
    format!("HALOID_{}", key.replace('.', "_").to_ascii_uppercase())
}

/// Every setting key, in order.
pub fn known_keys() -> Vec<String> {
    Settings::default().to_map().into_keys().collect()
}

/// The settings pinned by the environment, keyed by setting. Only known keys
/// are consulted, so unrelated `HALOID_*` variables are never mistaken for
/// settings.
pub fn overrides(lookup: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    known_keys()
        .into_iter()
        .filter_map(|key| lookup(&env_var(&key)).map(|value| (key, value)))
        .collect()
}

/// The stored rows with the environment laid over them.
pub fn layer(
    stored: &BTreeMap<String, String>,
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = stored.clone();
    merged.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
}

impl Settings {
    /// Check an administrator's write before it is stored.
    ///
    /// Loading is lenient — a bad stored value falls back and warns — but a
    /// write is strict: an unknown key, a value that would not parse, or a
    /// key the environment pins is refused outright, because accepting it
    /// would leave the administrator believing a change took effect.
    pub fn check_write(
        stored: &BTreeMap<String, String>,
        env: &BTreeMap<String, String>,
        changes: &BTreeMap<String, String>,
    ) -> Result<(), Vec<SettingWarning>> {
        let known = known_keys();
        let mut problems = Vec::new();
        for key in changes.keys() {
            if !known.contains(key) {
                problems.push(SettingWarning {
                    key: key.clone(),
                    reason: "unknown setting".into(),
                });
            } else if env.contains_key(key) {
                problems.push(SettingWarning {
                    key: key.clone(),
                    reason: format!("set by the environment ({})", env_var(key)),
                });
            }
        }

        let mut proposed = stored.clone();
        proposed.extend(changes.iter().map(|(k, v)| (k.clone(), v.clone())));
        let load = Settings::from_map(&layer(&proposed, env));
        problems.extend(
            load.warnings
                .into_iter()
                .filter(|w| changes.contains_key(&w.key) && known.contains(&w.key)),
        );

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn variable_names_follow_the_key() {
        assert_eq!(
            env_var("uploads.max_size_user"),
            "HALOID_UPLOADS_MAX_SIZE_USER"
        );
        assert_eq!(env_var("site.base_url"), "HALOID_SITE_BASE_URL");
    }

    #[test]
    fn no_two_settings_share_a_variable() {
        let mut vars: Vec<String> = known_keys().iter().map(|k| env_var(k)).collect();
        let count = vars.len();
        vars.sort();
        vars.dedup();
        assert_eq!(vars.len(), count);
    }

    #[test]
    fn no_setting_collides_with_a_bootstrap_variable() {
        // These configure the process, not the settings, and must never be
        // read as one.
        for bootstrap in [
            "HALOID_BIND",
            "HALOID_DATA_DIR",
            "HALOID_LOG",
            "HALOID_LOG_FORMAT",
            "HALOID_DISABLE_JOBS",
        ] {
            for key in known_keys() {
                assert_ne!(env_var(&key), bootstrap, "{key}");
            }
        }
        for key in known_keys() {
            assert!(
                !env_var(&key).starts_with("HALOID_STORAGE_"),
                "{key} would shadow storage credentials"
            );
        }
    }

    #[test]
    fn only_known_settings_are_read_from_the_environment() {
        let env = overrides(|var| match var {
            "HALOID_SITE_NAME" => Some("From env".into()),
            "HALOID_BIND" => Some("0.0.0.0:80".into()),
            _ => None,
        });
        assert_eq!(env, map(&[("site.name", "From env")]));
    }

    #[test]
    fn the_environment_wins_over_the_database() {
        let stored = map(&[("site.name", "From db"), ("uploads.enabled", "false")]);
        let env = map(&[("site.name", "From env")]);
        let load = Settings::from_map(&layer(&stored, &env));
        assert_eq!(load.settings.site.name, "From env");
        assert!(
            !load.settings.uploads.enabled,
            "unpinned keys keep the db value"
        );
    }

    #[test]
    fn a_valid_write_passes() {
        let changes = map(&[("site.name", "Pictures"), ("uploads.enabled", "off")]);
        assert_eq!(
            Settings::check_write(&BTreeMap::new(), &BTreeMap::new(), &changes),
            Ok(())
        );
    }

    #[test]
    fn a_write_with_a_bad_value_or_unknown_key_is_refused() {
        let changes = map(&[
            ("uploads.max_size_user", "lots"),
            ("uploads.max_size_moderator", "1"),
            ("site.name", "Fine"),
        ]);
        let problems =
            Settings::check_write(&BTreeMap::new(), &BTreeMap::new(), &changes).unwrap_err();
        let keys: Vec<&str> = problems.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["uploads.max_size_moderator", "uploads.max_size_user"]
        );
    }

    #[test]
    fn a_write_to_a_pinned_key_is_refused_and_names_the_variable() {
        let env = map(&[("site.name", "From env")]);
        let problems =
            Settings::check_write(&BTreeMap::new(), &env, &map(&[("site.name", "x")])).unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].reason.contains("HALOID_SITE_NAME"),
            "{:?}",
            problems
        );
    }

    #[test]
    fn a_stale_bad_row_does_not_block_an_unrelated_write() {
        // The bad row is already there and already falling back; refusing
        // every other change until someone fixes it would be the worse
        // behaviour.
        let stored = map(&[("watermark.opacity", "250")]);
        assert_eq!(
            Settings::check_write(
                &stored,
                &BTreeMap::new(),
                &map(&[("site.name", "Pictures")])
            ),
            Ok(())
        );
    }

    #[test]
    fn a_write_breaking_a_cross_field_rule_is_refused() {
        let problems = Settings::check_write(
            &BTreeMap::new(),
            &BTreeMap::new(),
            &map(&[("derivatives.medium_threshold", "100")]),
        )
        .unwrap_err();
        assert_eq!(problems[0].key, "derivatives.medium_threshold");
    }
}
