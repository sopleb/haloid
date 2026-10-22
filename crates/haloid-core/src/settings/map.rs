//! Reading and writing settings as the flat key/value rows the database holds.
//!
//! The database stores one row per setting, so a nested [`Settings`] has to be
//! flattened on write and rebuilt on read. Both directions live here, pure and
//! testable, rather than in the persistence layer.
//!
//! Two governing rules. **A bad or missing value never fails the load:** it
//! falls back to the compiled default and records a [`SettingWarning`]. An
//! administrator who fat-fingers a number in the database should get a warning
//! in the log and a working server, not an instance that refuses to boot.
//!
//! And **a blank value means absent**, for every type including lists. So
//! blanking the extension allowlist restores the defaults rather than
//! rejecting every upload — the safer direction for a setting that could
//! otherwise take an instance offline by being emptied.

use super::{MediumDimension, ModerationMode, Settings, UpdateMode};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// A setting that could not be used as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingWarning {
    pub key: String,
    pub reason: String,
}

/// The result of rebuilding settings from stored rows.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsLoad {
    pub settings: Settings,
    pub warnings: Vec<SettingWarning>,
}

/// Reads typed values out of the stored map, falling back and warning rather
/// than failing, and tracking which keys it understood.
struct Reader<'a> {
    map: &'a BTreeMap<String, String>,
    warnings: Vec<SettingWarning>,
    known: BTreeSet<String>,
}

impl<'a> Reader<'a> {
    fn new(map: &'a BTreeMap<String, String>) -> Self {
        Self {
            map,
            warnings: Vec::new(),
            known: BTreeSet::new(),
        }
    }

    fn warn(&mut self, key: &str, reason: impl Into<String>) {
        self.warnings.push(SettingWarning {
            key: key.to_string(),
            reason: reason.into(),
        });
    }

    /// The raw value, if present and not blank.
    fn raw(&mut self, key: &str) -> Option<&'a str> {
        self.known.insert(key.to_string());
        self.map
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }

    fn bool(&mut self, key: &str, default: bool) -> bool {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                // Browsers, shells and hand-written SQL all spell booleans
                // differently; accept the lot rather than being principled.
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    self.warn(key, format!("{raw:?} is not a boolean"));
                    default
                }
            },
        }
    }

    fn integer<T>(&mut self, key: &str, default: T) -> T
    where
        T: std::str::FromStr + Copy,
    {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.parse::<T>() {
                Ok(value) => value,
                Err(_) => {
                    self.warn(key, format!("{raw:?} is not a number in range"));
                    default
                }
            },
        }
    }

    fn string(&mut self, key: &str, default: &str) -> String {
        self.raw(key).unwrap_or(default).to_string()
    }

    /// A comma-separated list. Entries are trimmed and blanks dropped, so
    /// `"jpg, png,,"` is two entries.
    ///
    /// A value that contains nothing but separators (`" , , "`) is not blank
    /// and so reaches here, but parses to no entries at all. That falls back
    /// to the default rather than yielding an empty list: an empty extension
    /// allowlist would reject every upload, and nobody types `" , , "` meaning
    /// to take their instance offline.
    fn list(&mut self, key: &str, default: &[String]) -> Vec<String> {
        match self.raw(key) {
            None => default.to_vec(),
            Some(raw) => {
                let entries: Vec<String> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|entry| !entry.is_empty())
                    .map(str::to_string)
                    .collect();
                if entries.is_empty() {
                    self.warn(key, format!("{raw:?} contains no entries"));
                    return default.to_vec();
                }
                entries
            }
        }
    }

    fn seconds(&mut self, key: &str, default: Duration) -> Duration {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.parse::<u64>() {
                Ok(secs) => Duration::from_secs(secs),
                Err(_) => {
                    self.warn(key, format!("{raw:?} is not a count of seconds"));
                    default
                }
            },
        }
    }

    fn update_mode(&mut self, key: &str, default: UpdateMode) -> UpdateMode {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "off" => UpdateMode::Off,
                "notify" => UpdateMode::Notify,
                "auto" => UpdateMode::Auto,
                _ => {
                    self.warn(key, format!("{raw:?} is not one of off, notify, auto"));
                    default
                }
            },
        }
    }

    /// An hour of the day, 0-23.
    fn hour(&mut self, key: &str, default: u8) -> u8 {
        let value = self.integer::<u16>(key, u16::from(default));
        if value > 23 {
            self.warn(key, format!("{value} is not an hour, 0-23"));
            return default;
        }
        u8::try_from(value).unwrap_or(default)
    }

    fn moderation_mode(&mut self, key: &str, default: ModerationMode) -> ModerationMode {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "off" => ModerationMode::Off,
                "guests" => ModerationMode::Guests,
                "all" => ModerationMode::All,
                _ => {
                    self.warn(key, format!("{raw:?} is not one of off, guests, all"));
                    default
                }
            },
        }
    }

    fn watermark_position(
        &mut self,
        key: &str,
        default: crate::processor::WatermarkPosition,
    ) -> crate::processor::WatermarkPosition {
        match self.raw(key) {
            None => default,
            Some(raw) => crate::processor::WatermarkPosition::parse(&raw.to_ascii_lowercase())
                .unwrap_or_else(|| {
                    self.warn(
                        key,
                        format!(
                            "{raw:?} is not one of top-left, top-right, bottom-left, bottom-right, center"
                        ),
                    );
                    default
                }),
        }
    }

    fn medium_dimension(&mut self, key: &str, default: MediumDimension) -> MediumDimension {
        match self.raw(key) {
            None => default,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "width" => MediumDimension::Width,
                "height" => MediumDimension::Height,
                "longest" => MediumDimension::Longest,
                _ => {
                    self.warn(key, format!("{raw:?} is not one of width, height, longest"));
                    default
                }
            },
        }
    }

    /// Percentages that would be meaningless outside 0-100.
    fn percent(&mut self, key: &str, default: u8) -> u8 {
        let value = self.integer::<u16>(key, u16::from(default));
        if value > 100 {
            self.warn(key, format!("{value} is outside 0-100"));
            return default;
        }
        u8::try_from(value).unwrap_or(default)
    }
}

impl ModerationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Guests => "guests",
            Self::All => "all",
        }
    }
}

impl Settings {
    /// Rebuild settings from stored rows, falling back to defaults for
    /// anything missing or unusable.
    pub fn from_map(map: &BTreeMap<String, String>) -> SettingsLoad {
        let d = Settings::default();
        let mut r = Reader::new(map);

        let mut settings = Settings {
            site: super::SiteSettings {
                name: r.string("site.name", &d.site.name),
                base_url: r
                    .string("site.base_url", &d.site.base_url)
                    .trim_end_matches('/')
                    .to_string(),
                maintenance: r.bool("site.maintenance", d.site.maintenance),
                registration_open: r.bool("site.registration_open", d.site.registration_open),
                registrations_per_hour: r
                    .integer("site.registrations_per_hour", d.site.registrations_per_hour)
                    .max(1),
                theme: theme_name(r.string("site.theme", &d.site.theme), &d.site.theme),
            },
            uploads: super::UploadSettings {
                enabled: r.bool("uploads.enabled", d.uploads.enabled),
                guests_enabled: r.bool("uploads.guests_enabled", d.uploads.guests_enabled),
                max_size_guest: r.integer("uploads.max_size_guest", d.uploads.max_size_guest),
                max_size_user: r.integer("uploads.max_size_user", d.uploads.max_size_user),
                max_size_manager: r.integer("uploads.max_size_manager", d.uploads.max_size_manager),
                max_size_admin: r.integer("uploads.max_size_admin", d.uploads.max_size_admin),
                max_pixels: r.integer("uploads.max_pixels", d.uploads.max_pixels),
                allowed_extensions: r
                    .list("uploads.allowed_extensions", &d.uploads.allowed_extensions)
                    .into_iter()
                    .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                    .collect(),
                max_expiration: r.seconds("uploads.max_expiration_secs", d.uploads.max_expiration),
                detect_duplicates: r.bool("uploads.detect_duplicates", d.uploads.detect_duplicates),
                allow_url_sources: r.bool("uploads.allow_url_sources", d.uploads.allow_url_sources),
                per_hour_guest: r
                    .integer("uploads.per_hour_guest", d.uploads.per_hour_guest)
                    .max(1),
                per_hour_user: r
                    .integer("uploads.per_hour_user", d.uploads.per_hour_user)
                    .max(1),
                stop_words: r.list("uploads.stop_words", &d.uploads.stop_words),
            },
            derivatives: super::DerivativeSettings {
                thumb_edge: r.integer("derivatives.thumb_edge", d.derivatives.thumb_edge),
                medium_edge: r.integer("derivatives.medium_edge", d.derivatives.medium_edge),
                medium_threshold: r.integer(
                    "derivatives.medium_threshold",
                    d.derivatives.medium_threshold,
                ),
                medium_fixed_dimension: r.medium_dimension(
                    "derivatives.medium_fixed_dimension",
                    d.derivatives.medium_fixed_dimension,
                ),
                animated_derivatives: r.bool(
                    "derivatives.animated_derivatives",
                    d.derivatives.animated_derivatives,
                ),
                strip_exif: r.bool("derivatives.strip_exif", d.derivatives.strip_exif),
            },
            moderation: super::ModerationSettings {
                mode: r.moderation_mode("moderation.mode", d.moderation.mode),
            },
            watermark: super::WatermarkSettings {
                enabled: r.bool("watermark.enabled", d.watermark.enabled),
                apply_to_guests: r.bool("watermark.apply_to_guests", d.watermark.apply_to_guests),
                apply_to_users: r.bool("watermark.apply_to_users", d.watermark.apply_to_users),
                apply_to_managers: r
                    .bool("watermark.apply_to_managers", d.watermark.apply_to_managers),
                apply_to_admins: r.bool("watermark.apply_to_admins", d.watermark.apply_to_admins),
                percentage: r.percent("watermark.percentage", d.watermark.percentage),
                opacity: r.percent("watermark.opacity", d.watermark.opacity),
                margin: r.integer("watermark.margin", d.watermark.margin),
                min_width: r.integer("watermark.min_width", d.watermark.min_width),
                min_height: r.integer("watermark.min_height", d.watermark.min_height),
                position: r.watermark_position("watermark.position", d.watermark.position),
            },
            content: super::ContentSettings {
                guests_see_nsfw: r.bool("content.guests_see_nsfw", d.content.guests_see_nsfw),
                default_nsfw: r.bool("content.default_nsfw", d.content.default_nsfw),
            },
            updates: super::UpdateSettings {
                mode: r.update_mode("updates.mode", d.updates.mode),
                hour: r.hour("updates.hour", d.updates.hour),
            },
        };

        // A medium threshold below the medium edge would produce a "medium"
        // larger than its source.
        if settings.derivatives.medium_threshold < settings.derivatives.medium_edge {
            r.warn(
                "derivatives.medium_threshold",
                format!(
                    "{} is below derivatives.medium_edge ({}); raising it",
                    settings.derivatives.medium_threshold, settings.derivatives.medium_edge
                ),
            );
            settings.derivatives.medium_threshold = settings.derivatives.medium_edge;
        }

        // Zero-sized derivatives are a configuration mistake, not a request
        // for no derivatives.
        if settings.derivatives.thumb_edge == 0 {
            r.warn("derivatives.thumb_edge", "must be greater than zero");
            settings.derivatives.thumb_edge = d.derivatives.thumb_edge;
        }
        if settings.derivatives.medium_edge == 0 {
            r.warn("derivatives.medium_edge", "must be greater than zero");
            settings.derivatives.medium_edge = d.derivatives.medium_edge;
            settings.derivatives.medium_threshold = d.derivatives.medium_threshold;
        }

        let mut warnings = std::mem::take(&mut r.warnings);
        for key in map.keys() {
            if !r.known.contains(key) {
                // Not an error: a key from a newer version, or a leftover from
                // an older one. Worth saying out loud so it is not mistaken
                // for having an effect.
                warnings.push(SettingWarning {
                    key: key.clone(),
                    reason: "unknown setting, ignored".into(),
                });
            }
        }

        SettingsLoad { settings, warnings }
    }

    /// Flatten to the stored form. Every key [`Settings::from_map`] reads
    /// appears here, so a round-trip is lossless.
    pub fn to_map(&self) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        let mut put = |key: &str, value: String| {
            map.insert(key.to_string(), value);
        };

        put("site.name", self.site.name.clone());
        put("site.base_url", self.site.base_url.clone());
        put("site.maintenance", bool_str(self.site.maintenance));
        put(
            "site.registration_open",
            bool_str(self.site.registration_open),
        );
        put(
            "site.registrations_per_hour",
            self.site.registrations_per_hour.to_string(),
        );
        put("site.theme", self.site.theme.clone());

        let u = &self.uploads;
        put("uploads.enabled", bool_str(u.enabled));
        put("uploads.guests_enabled", bool_str(u.guests_enabled));
        put("uploads.max_size_guest", u.max_size_guest.to_string());
        put("uploads.max_size_user", u.max_size_user.to_string());
        put("uploads.max_size_manager", u.max_size_manager.to_string());
        put("uploads.max_size_admin", u.max_size_admin.to_string());
        put("uploads.max_pixels", u.max_pixels.to_string());
        put("uploads.allowed_extensions", u.allowed_extensions.join(","));
        put(
            "uploads.max_expiration_secs",
            u.max_expiration.as_secs().to_string(),
        );
        put("uploads.detect_duplicates", bool_str(u.detect_duplicates));
        put("uploads.allow_url_sources", bool_str(u.allow_url_sources));
        put("uploads.per_hour_guest", u.per_hour_guest.to_string());
        put("uploads.per_hour_user", u.per_hour_user.to_string());
        put("uploads.stop_words", u.stop_words.join(","));

        let d = &self.derivatives;
        put("derivatives.thumb_edge", d.thumb_edge.to_string());
        put("derivatives.medium_edge", d.medium_edge.to_string());
        put(
            "derivatives.medium_threshold",
            d.medium_threshold.to_string(),
        );
        put(
            "derivatives.medium_fixed_dimension",
            d.medium_fixed_dimension.as_str().to_string(),
        );
        put(
            "derivatives.animated_derivatives",
            bool_str(d.animated_derivatives),
        );
        put("derivatives.strip_exif", bool_str(d.strip_exif));

        put("moderation.mode", self.moderation.mode.as_str().to_string());

        let w = &self.watermark;
        put("watermark.enabled", bool_str(w.enabled));
        put("watermark.apply_to_guests", bool_str(w.apply_to_guests));
        put("watermark.apply_to_users", bool_str(w.apply_to_users));
        put("watermark.apply_to_managers", bool_str(w.apply_to_managers));
        put("watermark.apply_to_admins", bool_str(w.apply_to_admins));
        put("watermark.percentage", w.percentage.to_string());
        put("watermark.opacity", w.opacity.to_string());
        put("watermark.margin", w.margin.to_string());
        put("watermark.min_width", w.min_width.to_string());
        put("watermark.min_height", w.min_height.to_string());
        put("watermark.position", w.position.as_str().to_string());

        put(
            "content.guests_see_nsfw",
            bool_str(self.content.guests_see_nsfw),
        );
        put("content.default_nsfw", bool_str(self.content.default_nsfw));
        put("updates.mode", self.updates.mode.as_str().to_string());
        put("updates.hour", self.updates.hour.to_string());

        map
    }
}

fn bool_str(value: bool) -> String {
    if value { "true" } else { "false" }.to_string()
}

/// A theme is named, not described: lowercase letters, digits and dashes,
/// short. Anything else is ignored, so a setting can never put arbitrary
/// text into the page's markup.
fn theme_name(value: String, default: &str) -> String {
    let ok = !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { value } else { default.to_string() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_theme_is_a_short_lowercase_name_or_the_default() {
        let load = |value: &str| {
            let map = BTreeMap::from([("site.theme".to_string(), value.to_string())]);
            Settings::from_map(&map).settings.site.theme
        };
        assert_eq!(load("day"), "day");
        assert_eq!(load("high-contrast-2"), "high-contrast-2");
        assert_eq!(load("Night"), "night");
        assert_eq!(load("\"><script>"), "night");
        assert_eq!(load(""), "night");
        assert_eq!(load(&"a".repeat(33)), "night");
        assert_eq!(Settings::default().site.theme, "night");
    }

    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn an_empty_map_yields_the_defaults_without_warnings() {
        let load = Settings::from_map(&BTreeMap::new());
        assert_eq!(load.settings, Settings::default());
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    }

    #[test]
    fn a_round_trip_is_lossless() {
        // Every key read is also written, so an administrator saving unchanged
        // settings cannot silently lose one.
        let original = Settings::default();
        let load = Settings::from_map(&original.to_map());
        assert_eq!(load.settings, original);
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    }

    #[test]
    fn a_modified_round_trip_is_lossless() {
        let mut original = Settings::default();
        original.site.name = "Pictures".into();
        original.uploads.enabled = false;
        original.uploads.max_size_user = 12_345;
        original.uploads.allowed_extensions = vec!["jpg".into(), "webp".into()];
        original.uploads.stop_words = vec!["spam".into(), "scam".into()];
        original.uploads.max_expiration = Duration::from_secs(600);
        original.moderation.mode = ModerationMode::All;
        original.watermark.enabled = true;
        original.watermark.opacity = 33;
        original.derivatives.medium_edge = 800;
        original.derivatives.medium_threshold = 800;
        original.derivatives.medium_fixed_dimension = MediumDimension::Longest;

        let load = Settings::from_map(&original.to_map());
        assert_eq!(load.settings, original);
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    }

    #[test]
    fn booleans_accept_every_spelling_clients_actually_send() {
        for truthy in ["1", "true", "TRUE", "yes", "on", " true "] {
            let load = Settings::from_map(&map(&[("site.maintenance", truthy)]));
            assert!(load.settings.site.maintenance, "{truthy:?} read as false");
            assert!(load.warnings.is_empty(), "{truthy:?} warned");
        }
        for falsy in ["0", "false", "FALSE", "no", "off"] {
            let load = Settings::from_map(&map(&[("site.maintenance", falsy)]));
            assert!(!load.settings.site.maintenance, "{falsy:?} read as true");
            assert!(load.warnings.is_empty(), "{falsy:?} warned");
        }
    }

    #[test]
    fn a_bad_value_falls_back_and_warns_rather_than_failing() {
        // The whole point: a fat-fingered row must not stop the server booting.
        let load = Settings::from_map(&map(&[
            ("site.maintenance", "maybe"),
            ("uploads.max_size_user", "ten megabytes"),
            ("moderation.mode", "sometimes"),
            ("watermark.opacity", "250"),
            ("derivatives.medium_fixed_dimension", "diagonal"),
        ]));

        let d = Settings::default();
        assert_eq!(load.settings.site.maintenance, d.site.maintenance);
        assert_eq!(load.settings.uploads.max_size_user, d.uploads.max_size_user);
        assert_eq!(load.settings.moderation.mode, d.moderation.mode);
        assert_eq!(load.settings.watermark.opacity, d.watermark.opacity);
        assert_eq!(
            load.settings.derivatives.medium_fixed_dimension,
            d.derivatives.medium_fixed_dimension
        );

        let keys: Vec<&str> = load.warnings.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "site.maintenance",
                "uploads.max_size_user",
                "derivatives.medium_fixed_dimension",
                "moderation.mode",
                "watermark.opacity"
            ]
        );
    }

    #[test]
    fn a_blank_value_is_treated_as_absent() {
        let load = Settings::from_map(&map(&[("site.name", "   "), ("uploads.enabled", "")]));
        let d = Settings::default();
        assert_eq!(load.settings.site.name, d.site.name);
        assert_eq!(load.settings.uploads.enabled, d.uploads.enabled);
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    }

    #[test]
    fn unknown_keys_are_reported_but_ignored() {
        let load = Settings::from_map(&map(&[
            ("site.name", "Pictures"),
            ("uploads.max_size_moderator", "1"),
        ]));
        assert_eq!(load.settings.site.name, "Pictures");
        assert_eq!(load.warnings.len(), 1);
        assert_eq!(load.warnings[0].key, "uploads.max_size_moderator");
        assert!(load.warnings[0].reason.contains("unknown"));
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_is_removed() {
        // Every emitted URL is built by concatenation, so a trailing slash
        // would produce `//` in every link we hand a client.
        let load = Settings::from_map(&map(&[("site.base_url", "https://img.example.com/")]));
        assert_eq!(load.settings.site.base_url, "https://img.example.com");
    }

    #[test]
    fn extensions_are_normalised() {
        let load = Settings::from_map(&map(&[(
            "uploads.allowed_extensions",
            " .JPG, png ,,WEBP,",
        )]));
        assert_eq!(
            load.settings.uploads.allowed_extensions,
            vec!["jpg", "png", "webp"]
        );
        assert!(load.settings.extension_allowed(".Jpg"));
    }

    #[test]
    fn a_medium_threshold_below_the_medium_edge_is_raised() {
        let load = Settings::from_map(&map(&[
            ("derivatives.medium_edge", "900"),
            ("derivatives.medium_threshold", "100"),
        ]));
        assert_eq!(load.settings.derivatives.medium_threshold, 900);
        assert_eq!(load.warnings.len(), 1);
        assert_eq!(load.warnings[0].key, "derivatives.medium_threshold");
    }

    #[test]
    fn zero_sized_derivatives_fall_back() {
        let load = Settings::from_map(&map(&[
            ("derivatives.thumb_edge", "0"),
            ("derivatives.medium_edge", "0"),
        ]));
        let d = Settings::default();
        assert_eq!(
            load.settings.derivatives.thumb_edge,
            d.derivatives.thumb_edge
        );
        assert_eq!(
            load.settings.derivatives.medium_edge,
            d.derivatives.medium_edge
        );
        assert_eq!(load.warnings.len(), 2);
    }

    #[test]
    fn update_settings_default_to_notify_at_four_and_refuse_nonsense() {
        let d = Settings::default();
        assert_eq!((d.updates.mode, d.updates.hour), (UpdateMode::Notify, 4));
        let read = |pairs: &[(&str, &str)]| {
            let map: BTreeMap<String, String> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            Settings::from_map(&map)
        };
        let load = read(&[("updates.mode", "auto"), ("updates.hour", "23")]);
        assert_eq!(
            (load.settings.updates.mode, load.settings.updates.hour),
            (UpdateMode::Auto, 23)
        );
        assert!(load.warnings.is_empty());
        let load = read(&[("updates.mode", "sometimes"), ("updates.hour", "24")]);
        assert_eq!(load.settings.updates, d.updates);
        assert_eq!(load.warnings.len(), 2);
        for mode in [UpdateMode::Off, UpdateMode::Notify, UpdateMode::Auto] {
            let mut s = Settings::default();
            s.updates.mode = mode;
            assert_eq!(Settings::from_map(&s.to_map()).settings.updates.mode, mode);
        }
    }

    #[test]
    fn moderation_modes_round_trip_through_their_wire_names() {
        for mode in [
            ModerationMode::Off,
            ModerationMode::Guests,
            ModerationMode::All,
        ] {
            let load = Settings::from_map(&map(&[("moderation.mode", mode.as_str())]));
            assert_eq!(load.settings.moderation.mode, mode);
        }
    }

    #[test]
    fn a_blank_list_falls_back_to_the_default() {
        // Blank means absent for every type, lists included. Asserted against
        // allowed_extensions because its default is non-empty, so the two
        // outcomes are actually distinguishable here — with stop_words, whose
        // default is empty, this test would pass either way and prove nothing.
        //
        // The consequence is deliberate: blanking the extension list does not
        // disable all uploads, it restores the defaults. Someone wanting to
        // accept nothing has to say so another way, which is the safer
        // direction for a setting that can take an instance offline.
        let load = Settings::from_map(&map(&[("uploads.allowed_extensions", "")]));
        assert_eq!(
            load.settings.uploads.allowed_extensions,
            Settings::default().uploads.allowed_extensions
        );
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    }

    #[test]
    fn a_list_with_only_separators_also_falls_back() {
        let load = Settings::from_map(&map(&[("uploads.allowed_extensions", " , , ")]));
        assert!(
            !load.settings.uploads.allowed_extensions.is_empty(),
            "a list of separators emptied the allowlist"
        );
    }
}
