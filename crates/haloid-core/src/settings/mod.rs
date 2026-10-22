//! Operational settings.
//!
//! Three layers, highest wins: these compiled defaults, then the `ih_settings`
//! table, then bootstrap environment variables. A missing row is never an
//! error — [`Settings::default`] is always a valid running configuration, so a
//! fresh install boots and a bad value falls back rather than taking the
//! server down.
//!
//! This module is plain data. Reading and writing the flat key/value form the
//! database stores is [`map`]; the environment layer and write validation are
//! [`env`]; persistence and the hot-reload handle live in `haloid-db` and
//! `haloid-service`.

pub mod env;
pub mod map;

pub use map::{SettingWarning, SettingsLoad};

use crate::auth::{Actor, Role};
use std::time::Duration;

/// Everything an administrator can change at runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub site: SiteSettings,
    pub uploads: UploadSettings,
    pub derivatives: DerivativeSettings,
    pub moderation: ModerationSettings,
    pub watermark: WatermarkSettings,
    pub content: ContentSettings,
    pub updates: UpdateSettings,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SiteSettings {
    pub name: String,
    /// Public base URL, no trailing slash. Every emitted URL is built from it.
    pub base_url: String,
    pub maintenance: bool,
    /// Whether anyone may register an account.
    pub registration_open: bool,
    /// Registrations allowed from one address per hour, as a burst that
    /// refills evenly. At least 1.
    pub registrations_per_hour: u32,
    /// The web app's theme for anyone who has not chosen their own: a
    /// short lowercase name. The app falls back to its default for a name
    /// it does not know.
    pub theme: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadSettings {
    pub enabled: bool,
    pub guests_enabled: bool,
    pub max_size_guest: u64,
    pub max_size_user: u64,
    pub max_size_manager: u64,
    pub max_size_admin: u64,
    /// Decompression-bomb guard, checked before any full decode.
    pub max_pixels: u64,
    pub allowed_extensions: Vec<String>,
    /// Longest expiration an upload may request; larger values are capped.
    pub max_expiration: Duration,
    pub detect_duplicates: bool,
    /// URL sources, fetched through the SSRF-hardened client. Off by
    /// default: a URL upload makes the server send a request for a stranger.
    pub allow_url_sources: bool,
    pub stop_words: Vec<String>,
    /// Uploads one address may make per hour as a guest, and one account
    /// as a user. Moderators and administrators are not limited. At least 1.
    pub per_hour_guest: u32,
    pub per_hour_user: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DerivativeSettings {
    pub thumb_edge: u32,
    pub medium_edge: u32,
    pub medium_threshold: u32,
    /// Which side the threshold is measured on.
    pub medium_fixed_dimension: MediumDimension,
    pub animated_derivatives: bool,
    /// Strip metadata from the stored file at upload time. Never per request:
    /// stripping on the fly costs static file serving, range requests and any
    /// CDN caching.
    pub strip_exif: bool,
}

/// Which side of an image the medium threshold is measured against.
///
/// Upstream measures width, so a tall 400x2000 image gets no medium there;
/// `Longest` gives portrait images one. `Width` is the default so that legacy
/// chain values match upstream out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediumDimension {
    Width,
    Height,
    Longest,
}

impl MediumDimension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
            Self::Longest => "longest",
        }
    }

    /// The measured side of a `width` x `height` image.
    pub const fn measure(self, width: u32, height: u32) -> u32 {
        match self {
            Self::Width => width,
            Self::Height => height,
            Self::Longest => {
                if width > height {
                    width
                } else {
                    height
                }
            }
        }
    }
}

/// Which uploads need approval before their bytes are served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationMode {
    Off,
    /// Guest uploads only.
    Guests,
    /// Everything from anyone who cannot moderate.
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModerationSettings {
    pub mode: ModerationMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatermarkSettings {
    pub enabled: bool,
    pub apply_to_guests: bool,
    pub apply_to_users: bool,
    pub apply_to_managers: bool,
    pub apply_to_admins: bool,
    /// Mark width as a percentage of the target's width.
    pub percentage: u8,
    pub opacity: u8,
    pub margin: u32,
    /// Skip entirely when the target is smaller than this.
    pub min_width: u32,
    pub min_height: u32,
    pub position: crate::processor::WatermarkPosition,
}

impl WatermarkSettings {
    /// Whether an upload by `actor` gets the mark.
    pub fn applies_to(&self, actor: crate::auth::Actor) -> bool {
        use crate::auth::Role;
        self.enabled
            && match actor.role() {
                None => self.apply_to_guests,
                Some(Role::User) => self.apply_to_users,
                Some(Role::Manager) => self.apply_to_managers,
                Some(Role::Admin) => self.apply_to_admins,
            }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContentSettings {
    /// Whether guests may view files flagged not-safe-for-work.
    pub guests_see_nsfw: bool,
    pub default_nsfw: bool,
}

/// Whether this instance looks for new releases, and what it does with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    /// No checks at all.
    Off,
    /// Check, and show administrators what is available.
    Notify,
    /// Check, and install a new release at [`UpdateSettings::hour`].
    Auto,
}

impl UpdateMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Notify => "notify",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateSettings {
    pub mode: UpdateMode,
    /// The hour, UTC, at which `auto` installs.
    pub hour: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            site: SiteSettings {
                name: "Haloid".into(),
                base_url: "http://localhost:8080".into(),
                maintenance: false,
                registration_open: true,
                registrations_per_hour: 6,
                theme: "night".into(),
            },
            uploads: UploadSettings {
                enabled: true,
                guests_enabled: true,
                max_size_guest: 16 * MIB,
                max_size_user: 64 * MIB,
                max_size_manager: 256 * MIB,
                max_size_admin: 1024 * MIB,
                max_pixels: 100_000_000,
                allowed_extensions: [
                    "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "heif", "mp4", "webm",
                    "mov", "mp3", "m4a", "wav",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
                max_expiration: Duration::from_secs(365 * 86_400),
                detect_duplicates: true,
                // Enabled only once the SSRF-hardened fetcher exists.
                allow_url_sources: false,
                per_hour_guest: 60,
                per_hour_user: 600,
                stop_words: Vec::new(),
            },
            derivatives: DerivativeSettings {
                thumb_edge: 160,
                medium_edge: 500,
                medium_threshold: 500,
                medium_fixed_dimension: MediumDimension::Width,
                animated_derivatives: false,
                strip_exif: true,
            },
            moderation: ModerationSettings {
                mode: ModerationMode::Off,
            },
            watermark: WatermarkSettings {
                enabled: false,
                apply_to_guests: true,
                apply_to_users: true,
                apply_to_managers: false,
                apply_to_admins: false,
                percentage: 10,
                opacity: 70,
                margin: 10,
                min_width: 200,
                min_height: 200,
                position: crate::processor::WatermarkPosition::BottomRight,
            },
            content: ContentSettings {
                guests_see_nsfw: false,
                default_nsfw: false,
            },
            updates: UpdateSettings {
                mode: UpdateMode::Notify,
                hour: 4,
            },
        }
    }
}

const MIB: u64 = 1024 * 1024;

impl Settings {
    /// The upload size limit that applies to this actor.
    pub fn max_upload_size(&self, actor: Actor) -> u64 {
        let u = &self.uploads;
        match actor.role() {
            None => u.max_size_guest,
            Some(Role::User) => u.max_size_user,
            Some(Role::Manager) => u.max_size_manager,
            Some(Role::Admin) => u.max_size_admin,
        }
    }

    /// Whether this actor may upload at all.
    pub fn may_upload(&self, actor: Actor) -> bool {
        if !self.uploads.enabled {
            // Administrators keep working while uploads are off for everyone
            // else, so an instance can be drained without locking its owner
            // out.
            return actor.is_admin();
        }
        if actor.is_guest() {
            return self.uploads.guests_enabled;
        }
        true
    }

    /// Whether an upload by this actor is held for approval.
    pub fn needs_moderation(&self, actor: Actor) -> bool {
        // Anyone who can moderate bypasses the queue; otherwise they would be
        // approving their own uploads.
        if actor.can_moderate() {
            return false;
        }
        match self.moderation.mode {
            ModerationMode::Off => false,
            ModerationMode::Guests => actor.is_guest(),
            ModerationMode::All => true,
        }
    }

    /// Whether an upload by this actor gets a watermark.
    pub fn watermarks(&self, actor: Actor) -> bool {
        if !self.watermark.enabled {
            return false;
        }
        let w = &self.watermark;
        match actor.role() {
            None => w.apply_to_guests,
            Some(Role::User) => w.apply_to_users,
            Some(Role::Manager) => w.apply_to_managers,
            Some(Role::Admin) => w.apply_to_admins,
        }
    }

    /// Whether an extension is accepted. Case-insensitive, dot optional.
    pub fn extension_allowed(&self, extension: &str) -> bool {
        let needle = extension.trim_start_matches('.').to_ascii_lowercase();
        self.uploads
            .allowed_extensions
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::UserId;

    fn user(role: Role) -> Actor {
        Actor::User {
            id: UserId(1),
            role,
        }
    }

    #[test]
    fn defaults_are_a_valid_running_configuration() {
        let s = Settings::default();
        assert!(s.uploads.enabled);
        assert!(!s.site.maintenance);
        assert!(s.may_upload(Actor::Guest));
        assert!(s.may_upload(user(Role::User)));
        assert!(!s.needs_moderation(Actor::Guest));
        assert!(!s.watermarks(Actor::Guest));
    }

    #[test]
    fn svg_is_not_an_allowed_extension_by_default() {
        // Stored XSS served from our own origin. libvips would happily
        // rasterise it, which is not a reason to accept it.
        let s = Settings::default();
        assert!(!s.extension_allowed("svg"));
        assert!(!s.extension_allowed(".svg"));
        assert!(!s.extension_allowed("SVG"));
    }

    #[test]
    fn extension_matching_ignores_case_and_a_leading_dot() {
        let s = Settings::default();
        for input in ["jpg", ".jpg", "JPG", ".JPG", "Jpg"] {
            assert!(s.extension_allowed(input), "{input:?} was rejected");
        }
        for input in ["php", "html", "exe", "", "jpg.php"] {
            assert!(!s.extension_allowed(input), "{input:?} was accepted");
        }
    }

    #[test]
    fn size_limits_rise_with_privilege() {
        let s = Settings::default();
        let guest = s.max_upload_size(Actor::Guest);
        let member = s.max_upload_size(user(Role::User));
        let manager = s.max_upload_size(user(Role::Manager));
        let admin = s.max_upload_size(user(Role::Admin));
        assert!(guest < member && member < manager && manager < admin);
    }

    #[test]
    fn disabling_uploads_still_lets_an_admin_in() {
        let mut s = Settings::default();
        s.uploads.enabled = false;
        assert!(!s.may_upload(Actor::Guest));
        assert!(!s.may_upload(user(Role::User)));
        assert!(!s.may_upload(user(Role::Manager)));
        assert!(s.may_upload(user(Role::Admin)));
    }

    #[test]
    fn disabling_guest_uploads_leaves_members_alone() {
        let mut s = Settings::default();
        s.uploads.guests_enabled = false;
        assert!(!s.may_upload(Actor::Guest));
        assert!(s.may_upload(user(Role::User)));
    }

    #[test]
    fn moderation_modes_select_the_right_actors() {
        let mut s = Settings::default();

        s.moderation.mode = ModerationMode::Off;
        assert!(!s.needs_moderation(Actor::Guest));
        assert!(!s.needs_moderation(user(Role::User)));

        s.moderation.mode = ModerationMode::Guests;
        assert!(s.needs_moderation(Actor::Guest));
        assert!(!s.needs_moderation(user(Role::User)));

        s.moderation.mode = ModerationMode::All;
        assert!(s.needs_moderation(Actor::Guest));
        assert!(s.needs_moderation(user(Role::User)));
    }

    #[test]
    fn moderators_never_queue_their_own_uploads() {
        let mut s = Settings::default();
        s.moderation.mode = ModerationMode::All;
        assert!(!s.needs_moderation(user(Role::Manager)));
        assert!(!s.needs_moderation(user(Role::Admin)));
    }

    #[test]
    fn watermarks_are_off_unless_enabled_even_for_targeted_roles() {
        let mut s = Settings::default();
        assert!(s.watermark.apply_to_guests);
        assert!(!s.watermarks(Actor::Guest), "master switch was ignored");

        s.watermark.enabled = true;
        assert!(s.watermarks(Actor::Guest));
        assert!(s.watermarks(user(Role::User)));
        assert!(!s.watermarks(user(Role::Manager)));
        assert!(!s.watermarks(user(Role::Admin)));
    }

    #[test]
    fn the_medium_threshold_matches_the_medium_edge_by_default() {
        // If the threshold were below the edge we would produce a "medium"
        // larger than the source.
        let d = Settings::default().derivatives;
        assert_eq!(d.medium_threshold, d.medium_edge);
    }
}
