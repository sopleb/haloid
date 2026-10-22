//! What this instance may do beyond the Community edition.
//!
//! One value, held by the service and consulted wherever an edition
//! differs, instead of edition checks scattered through the code. It is
//! worked out from the installed licence (see `haloid-licence`) and the
//! time; nothing here knows how a key is checked.

use chrono::{DateTime, Utc};

/// The kinds of licence key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenceKind {
    /// Three months of everything Pro offers.
    Trial,
    /// Two years, by default.
    Standard,
    /// No expiry.
    Perpetual,
}

impl LicenceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trial => "trial",
            Self::Standard => "standard",
            Self::Perpetual => "perpetual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "trial" => Self::Trial,
            "standard" => Self::Standard,
            "perpetual" => Self::Perpetual,
            _ => return None,
        })
    }
}

/// Where the licence stands, for the admin area and the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenceState {
    /// No key, or none that is for this instance.
    None,
    Active,
    /// Past its expiry date.
    Lapsed,
    /// Withdrawn by the licence service (a refund, a moved key): treated as
    /// a lapsed trial — branding back, no new Pro use, nothing stopped.
    Revoked,
}

impl LicenceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Active => "active",
            Self::Lapsed => "lapsed",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entitlements {
    /// Pro features may be put to new use: audio uploads, adding external
    /// storage. Storage already configured keeps working regardless, so a
    /// lapse never stops uploads or breaks a link.
    pub pro: bool,
    /// "Powered by Haloid" is shown.
    pub branding: bool,
    pub state: LicenceState,
}

impl Entitlements {
    /// No licence: the Community edition.
    pub const COMMUNITY: Self = Self {
        pro: false,
        branding: true,
        state: LicenceState::None,
    };

    /// A licence the licence service has withdrawn.
    pub const REVOKED: Self = Self {
        pro: false,
        branding: true,
        state: LicenceState::Revoked,
    };

    /// The rules, for a licence of `kind` that expires at `expires_at`.
    ///
    /// - Active: everything Pro, no branding. Trials included.
    /// - A lapsed paid key brings the branding back and nothing else.
    /// - A lapsed trial brings the branding back and stops new Pro use;
    ///   otherwise three months would buy Pro for good.
    pub fn for_licence(
        kind: LicenceKind,
        expires_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        let lapsed = expires_at.is_some_and(|at| now >= at);
        match (lapsed, kind) {
            (false, _) => Self {
                pro: true,
                branding: false,
                state: LicenceState::Active,
            },
            (true, LicenceKind::Trial) => Self {
                pro: false,
                branding: true,
                state: LicenceState::Lapsed,
            },
            (true, _) => Self {
                pro: true,
                branding: true,
                state: LicenceState::Lapsed,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn an_active_key_of_any_kind_is_pro_without_branding() {
        for kind in [LicenceKind::Trial, LicenceKind::Standard] {
            let e = Entitlements::for_licence(kind, Some(at(2030)), at(2029));
            assert!(e.pro && !e.branding, "{kind:?}");
            assert_eq!(e.state, LicenceState::Active);
        }
        let forever = Entitlements::for_licence(LicenceKind::Perpetual, None, at(2999));
        assert!(forever.pro && !forever.branding);
    }

    #[test]
    fn a_lapsed_paid_key_only_brings_the_branding_back() {
        let e = Entitlements::for_licence(LicenceKind::Standard, Some(at(2030)), at(2030));
        assert!(e.pro, "Pro features keep working");
        assert!(e.branding);
        assert_eq!(e.state, LicenceState::Lapsed);
    }

    #[test]
    fn a_lapsed_trial_stops_new_pro_use() {
        let e = Entitlements::for_licence(LicenceKind::Trial, Some(at(2030)), at(2031));
        assert!(!e.pro && e.branding);
        assert_eq!(e.state, LicenceState::Lapsed);
    }

    const _COMMUNITY_IS_BRANDED_AND_NOT_PRO: () =
        assert!(!Entitlements::COMMUNITY.pro && Entitlements::COMMUNITY.branding);
}
