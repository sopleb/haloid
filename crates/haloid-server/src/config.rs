//! Bootstrap configuration: what the process needs before it can read its
//! settings from the database. Environment only, and deliberately short;
//! everything operational is a runtime setting.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub database_url: String,
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    /// A `tracing` filter: `info,haloid=debug,sqlx=warn`.
    pub log_filter: String,
    pub log_json: bool,
    /// The escape hatch for running a second process, or debugging, without
    /// the background job runner.
    pub disable_jobs: bool,
    /// Signs ALTCHA challenges. Generated at startup when absent.
    pub altcha_secret: Option<String>,
    /// PBKDF2 iterations per ALTCHA attempt, replacing the design's 5000.
    /// For test suites that drive a real browser; lowering it in production
    /// makes the proof-of-work cheaper for an attacker too.
    pub altcha_cost: Option<u32>,
    /// The built single-page app. Served if the directory exists.
    pub frontend_dir: PathBuf,
    /// Reverse proxies whose `X-Forwarded-For` is believed: addresses or
    /// CIDR ranges, comma-separated. Empty: the peer address is the client.
    pub trusted_proxies: Vec<ipnet::IpNet>,
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl Config {
    /// Read from `lookup` — the process environment in production. The one
    /// required value is `DATABASE_URL`; every other has a working default.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let database_url =
            get("DATABASE_URL").ok_or("DATABASE_URL is not set: Haloid needs a MariaDB to run")?;
        let bind = match get("HALOID_BIND") {
            Some(bind) => bind
                .parse()
                .map_err(|_| format!("HALOID_BIND {bind:?} is not an address:port"))?,
            None => SocketAddr::from(([127, 0, 0, 1], 8080)),
        };
        let altcha_cost = get("HALOID_ALTCHA_COST")
            .map(|cost| {
                cost.parse::<u32>().ok().filter(|c| *c > 0).ok_or(format!(
                    "HALOID_ALTCHA_COST {cost:?} is not a positive number"
                ))
            })
            .transpose()?;
        let trusted_proxies = match get("HALOID_TRUSTED_PROXIES") {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|entry| {
                    entry
                        .parse::<ipnet::IpNet>()
                        .or_else(|_| entry.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                        .map_err(|_| {
                            format!(
                                "HALOID_TRUSTED_PROXIES: {entry:?} is not an address or CIDR range"
                            )
                        })
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(Self {
            database_url,
            bind,
            data_dir: get("HALOID_DATA_DIR").map_or_else(|| PathBuf::from("./data"), PathBuf::from),
            log_filter: get("HALOID_LOG").unwrap_or_else(|| "info".into()),
            log_json: get("HALOID_LOG_FORMAT").is_some_and(|f| f.eq_ignore_ascii_case("json")),
            disable_jobs: get("HALOID_DISABLE_JOBS").is_some_and(|v| truthy(&v)),
            altcha_secret: get("HALOID_ALTCHA_SECRET"),
            altcha_cost,
            frontend_dir: get("HALOID_FRONTEND_DIR")
                .map_or_else(|| PathBuf::from("./frontend/build"), PathBuf::from),
            trusted_proxies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn only_the_database_is_required() {
        let config = Config::from_env(env(&[("DATABASE_URL", "mysql://x@db/haloid")])).unwrap();
        assert_eq!(config.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("./data"));
        assert_eq!(config.log_filter, "info");
        assert!(!config.log_json);
        assert!(!config.disable_jobs);
        assert_eq!(config.altcha_secret, None);
        assert_eq!(config.altcha_cost, None);
        assert_eq!(config.frontend_dir, PathBuf::from("./frontend/build"));
        assert!(config.trusted_proxies.is_empty());
    }

    #[test]
    fn trusted_proxies_are_addresses_or_ranges() {
        let config = Config::from_env(env(&[
            ("DATABASE_URL", "mysql://x@db/haloid"),
            ("HALOID_TRUSTED_PROXIES", "127.0.0.1, 10.0.0.0/8 ,::1"),
        ]))
        .unwrap();
        assert_eq!(config.trusted_proxies.len(), 3);
        assert!(
            Config::from_env(env(&[
                ("DATABASE_URL", "mysql://x@db/haloid"),
                ("HALOID_TRUSTED_PROXIES", "my-proxy"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn a_missing_database_says_so() {
        let error = Config::from_env(env(&[])).unwrap_err();
        assert!(error.contains("DATABASE_URL"), "{error}");
        assert!(
            Config::from_env(env(&[("DATABASE_URL", "  ")])).is_err(),
            "blank is unset"
        );
    }

    #[test]
    fn every_variable_is_read() {
        let config = Config::from_env(env(&[
            ("DATABASE_URL", "mysql://x@db/haloid"),
            ("HALOID_BIND", "0.0.0.0:9000"),
            ("HALOID_DATA_DIR", "/srv/haloid"),
            ("HALOID_LOG", "debug"),
            ("HALOID_LOG_FORMAT", "JSON"),
            ("HALOID_DISABLE_JOBS", "1"),
            ("HALOID_ALTCHA_SECRET", "s3cret"),
            ("HALOID_ALTCHA_COST", "1"),
        ]))
        .unwrap();
        assert_eq!(config.bind, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/srv/haloid"));
        assert_eq!(config.log_filter, "debug");
        assert!(config.log_json && config.disable_jobs);
        assert_eq!(config.altcha_secret.as_deref(), Some("s3cret"));
        assert_eq!(config.altcha_cost, Some(1));
    }

    #[test]
    fn a_bad_altcha_cost_is_an_error() {
        for cost in ["0", "-1", "lots"] {
            let error = Config::from_env(env(&[
                ("DATABASE_URL", "mysql://x@db/haloid"),
                ("HALOID_ALTCHA_COST", cost),
            ]))
            .unwrap_err();
            assert!(error.contains("HALOID_ALTCHA_COST"), "{error}");
        }
    }

    #[test]
    fn a_bad_bind_address_is_an_error() {
        let error = Config::from_env(env(&[
            ("DATABASE_URL", "mysql://x@db/haloid"),
            ("HALOID_BIND", "localhost"),
        ]))
        .unwrap_err();
        assert!(error.contains("HALOID_BIND"), "{error}");
    }
}
