//! Building storage keys, and refusing unsafe ones.
//!
//! A key is a relative, `/`-separated path within a storage:
//! `2026/10/21/cat-ab12cd34.th.jpg`. Every key this crate writes or reads
//! goes through [`validate_key`] first, so no combination of user-supplied
//! filename, template and id can reach outside the storage's root.

use chrono::{DateTime, Datelike, Utc};
use haloid_core::StorageError;
use haloid_core::ids::UserId;
use haloid_core::media::Derivative;

/// The longest key, matching `ih_files.storage_path`.
pub const MAX_KEY_LEN: usize = 512;

/// The longest stored basename, in bytes, before its random suffix. Leaves
/// room for the suffix, a derivative infix and an extension inside the
/// 255-byte filename limit of every common filesystem.
pub const MAX_NAME_BYTES: usize = 100;

fn invalid(reason: impl Into<String>) -> StorageError {
    StorageError::InvalidKey(reason.into())
}

/// Check that `key` is safe to join onto a storage root: relative, no empty,
/// `.` or `..` components, no backslashes or NULs, and not too long.
pub fn validate_key(key: &str) -> Result<(), StorageError> {
    if key.is_empty() {
        return Err(invalid("empty key"));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(invalid(format!("key longer than {MAX_KEY_LEN} bytes")));
    }
    if key.contains(['\\', '\0']) {
        return Err(invalid("key contains a backslash or NUL"));
    }
    for component in key.split('/') {
        match component {
            "" => return Err(invalid("key has an empty component or a leading slash")),
            "." | ".." => return Err(invalid("key has a relative component")),
            _ if component.chars().any(char::is_control) => {
                return Err(invalid("key contains a control character"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// A stored basename: the uploaded filename's stem made safe, cut to
/// [`MAX_NAME_BYTES`] on a character boundary, then `-{suffix}`.
///
/// Letters and digits in any script survive; everything else becomes a
/// separator. The random suffix is what makes the name unique, so the stem
/// only has to be safe and recognisable, never exact.
pub fn stored_name(original_filename: &str, suffix: &str) -> String {
    let stem = original_filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem);

    let mut safe = String::new();
    let mut dash = false;
    for ch in stem.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            if dash && !safe.is_empty() {
                safe.push('-');
            }
            dash = false;
            if safe.len() + ch.len_utf8() > MAX_NAME_BYTES {
                break;
            }
            safe.push(ch);
        } else {
            dash = true;
        }
    }
    let safe = safe.trim_end_matches('-');
    let safe = if safe.is_empty() { "file" } else { safe };
    format!("{safe}-{suffix}")
}

/// What a key is built from.
#[derive(Debug, Clone, Copy)]
pub struct KeyParts<'a> {
    pub created_at: DateTime<Utc>,
    pub user_id: Option<UserId>,
    pub id_encoded: &'a str,
    /// From [`stored_name`].
    pub name: &'a str,
    /// Without a dot.
    pub extension: &'a str,
}

/// How a storage lays out its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// `YYYY/MM/DD/name.ext`, the default.
    DateFolder,
    /// `name.ext`.
    Direct,
    Template(Template),
}

/// A parsed `path` template: literal segments and `{placeholder}`s,
/// validated once when the storage is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(Vec<Piece>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Year,
    Month,
    Day,
    UserId,
    IdEncoded,
}

impl Template {
    /// Parse a template such as `{yyyy}/{user_id}`. Placeholders are
    /// `{yyyy}`, `{mm}`, `{dd}`, `{user_id}` (`guest` for guest uploads) and
    /// `{id_encoded}`; literal text is limited to ASCII letters, digits,
    /// `-`, `_` and `/`, so a template cannot smuggle in traversal.
    pub fn parse(template: &str) -> Result<Self, StorageError> {
        let mut pieces = Vec::new();
        let mut rest = template;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('{') {
                let (name, tail) = after
                    .split_once('}')
                    .ok_or_else(|| invalid("unclosed placeholder in path template"))?;
                pieces.push(match name {
                    "yyyy" => Piece::Year,
                    "mm" => Piece::Month,
                    "dd" => Piece::Day,
                    "user_id" => Piece::UserId,
                    "id_encoded" => Piece::IdEncoded,
                    other => return Err(invalid(format!("unknown placeholder {{{other}}}"))),
                });
                rest = tail;
            } else {
                let end = rest.find('{').unwrap_or(rest.len());
                let literal = &rest[..end];
                if !literal
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
                {
                    return Err(invalid(format!(
                        "path template literal {literal:?} has characters outside [A-Za-z0-9_/-]"
                    )));
                }
                pieces.push(Piece::Literal(literal.to_string()));
                rest = &rest[end..];
            }
        }
        let parsed = Self(pieces);
        // Prove it expands to a valid directory for some input.
        let probe = KeyParts {
            created_at: DateTime::UNIX_EPOCH,
            user_id: Some(UserId(1)),
            id_encoded: "probe",
            name: "probe",
            extension: "x",
        };
        validate_key(&join(&parsed.expand(&probe), "x"))?;
        Ok(parsed)
    }

    fn expand(&self, parts: &KeyParts<'_>) -> String {
        let date = parts.created_at;
        self.0
            .iter()
            .map(|piece| match piece {
                Piece::Literal(text) => text.clone(),
                Piece::Year => format!("{:04}", date.year()),
                Piece::Month => format!("{:02}", date.month()),
                Piece::Day => format!("{:02}", date.day()),
                Piece::UserId => parts
                    .user_id
                    .map_or_else(|| "guest".to_string(), |u| u.to_string()),
                Piece::IdEncoded => parts.id_encoded.to_string(),
            })
            .collect()
    }
}

/// A template's directory and a filename. A trailing slash on the directory
/// is forgiven and an empty directory is the root; a leading slash is left
/// in place for [`validate_key`] to refuse.
fn join(dir: &str, file: &str) -> String {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// The key for one derivative of an upload. Thumb, medium and frame share
/// the upload's directory and name, with `.th`, `.md` and `.fr` before the
/// extension, so they sort and move together.
pub fn object_key(
    layout: &Layout,
    parts: &KeyParts<'_>,
    derivative: Derivative,
) -> Result<String, StorageError> {
    if parts.extension.is_empty()
        || parts.extension.len() > 16
        || !parts.extension.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(invalid("extension must be 1-16 ASCII letters or digits"));
    }
    let file = format!("{}{}.{}", parts.name, derivative.suffix(), parts.extension);
    let key = match layout {
        Layout::Direct => file,
        Layout::DateFolder => {
            let d = parts.created_at;
            format!("{:04}/{:02}/{:02}/{file}", d.year(), d.month(), d.day())
        }
        Layout::Template(template) => join(&template.expand(parts), &file),
    };
    validate_key(&key)?;
    Ok(key)
}

/// The key of another derivative of the same upload, from its image key:
/// the same directory and stored name, the derivative's infix, and its own
/// extension. Derived from the stored key rather than rebuilt from the
/// layout, so it never depends on the clock at the moment of upload.
pub fn sibling_key(
    image_key: &str,
    derivative: Derivative,
    extension: &str,
) -> Result<String, StorageError> {
    validate_key(image_key)?;
    let (dir, file) = match image_key.rsplit_once('/') {
        Some((dir, file)) => (Some(dir), file),
        None => (None, image_key),
    };
    // Stored names never contain a dot (see `stored_name`), so the first one
    // ends the name.
    let name = file.split('.').next().unwrap_or(file);
    let sibling = format!("{name}{}.{extension}", derivative.suffix());
    let key = match dir {
        Some(dir) => format!("{dir}/{sibling}"),
        None => sibling,
    };
    validate_key(&key)?;
    Ok(key)
}

/// Percent-encode a key for a URL, segment by segment.
pub fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn parts<'a>(name: &'a str) -> KeyParts<'a> {
        KeyParts {
            created_at: Utc.with_ymd_and_hms(2026, 3, 7, 12, 0, 0).unwrap(),
            user_id: Some(UserId(42)),
            id_encoded: "aB3dE5fG7h",
            name,
            extension: "jpg",
        }
    }

    #[test]
    fn date_folder_keys_use_the_upload_date() {
        let key = object_key(&Layout::DateFolder, &parts("cat-x1"), Derivative::Image).unwrap();
        assert_eq!(key, "2026/03/07/cat-x1.jpg");
    }

    #[test]
    fn derivatives_get_their_infix() {
        let p = parts("cat-x1");
        let key = |d| object_key(&Layout::Direct, &p, d).unwrap();
        assert_eq!(key(Derivative::Image), "cat-x1.jpg");
        assert_eq!(key(Derivative::Thumb), "cat-x1.th.jpg");
        assert_eq!(key(Derivative::Medium), "cat-x1.md.jpg");
        assert_eq!(key(Derivative::Frame), "cat-x1.fr.jpg");
    }

    #[test]
    fn templates_expand_every_placeholder() {
        let t = Template::parse("u/{user_id}/{yyyy}-{mm}-{dd}/{id_encoded}").unwrap();
        let key = object_key(
            &Layout::Template(t.clone()),
            &parts("cat-x1"),
            Derivative::Image,
        )
        .unwrap();
        assert_eq!(key, "u/42/2026-03-07/aB3dE5fG7h/cat-x1.jpg");

        let mut guest = parts("cat-x1");
        guest.user_id = None;
        let key = object_key(&Layout::Template(t), &guest, Derivative::Image).unwrap();
        assert!(key.starts_with("u/guest/"), "{key}");
    }

    #[test]
    fn hostile_templates_are_refused_at_configuration_time() {
        for template in [
            "../{yyyy}",
            "/{yyyy}",
            "a/../b",
            "a//b",
            "{yyyy",
            "{nope}",
            "a\\b",
            "a b",
            "a.b",
            "%2e%2e",
        ] {
            assert!(Template::parse(template).is_err(), "{template:?} accepted");
        }
        assert!(Template::parse("").is_ok(), "an empty template is the root");
        assert!(
            Template::parse("{yyyy}/").is_ok(),
            "a trailing slash is forgiven"
        );
    }

    #[test]
    fn validate_key_rejects_every_escape() {
        for key in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "a/",
            "a\\..\\b",
            "a\0b",
            "a/\nb",
        ] {
            assert!(validate_key(key).is_err(), "{key:?} accepted");
        }
        assert!(validate_key(&"a".repeat(MAX_KEY_LEN + 1)).is_err());
        for key in [
            "a.jpg",
            "2026/01/02/cat-x.th.jpg",
            "猫/写真.jpg",
            "..hidden-is-a-name",
        ] {
            assert!(validate_key(key).is_ok(), "{key:?} rejected");
        }
    }

    #[test]
    fn hostile_extensions_are_refused() {
        for ext in ["", "j/pg", "../x", "jp g", &"a".repeat(17)] {
            let mut p = parts("cat-x1");
            p.extension = ext;
            assert!(
                object_key(&Layout::Direct, &p, Derivative::Image).is_err(),
                "{ext:?} accepted"
            );
        }
    }

    #[test]
    fn siblings_share_the_directory_and_name() {
        let image = "2026/03/07/cat-x1.heic";
        assert_eq!(
            sibling_key(image, Derivative::Thumb, "jpg").unwrap(),
            "2026/03/07/cat-x1.th.jpg"
        );
        assert_eq!(
            sibling_key(image, Derivative::Original, "heic").unwrap(),
            "2026/03/07/cat-x1.or.heic"
        );
        assert_eq!(
            sibling_key("cat-x1.jpg", Derivative::Frame, "jpg").unwrap(),
            "cat-x1.fr.jpg"
        );
        assert_eq!(
            sibling_key(image, Derivative::Image, "heic").unwrap(),
            image,
            "the image is its own sibling"
        );
        assert!(sibling_key("../x.jpg", Derivative::Thumb, "jpg").is_err());
    }

    #[test]
    fn stored_names_are_safe_and_recognisable() {
        assert_eq!(stored_name("Cat Photo.JPG", "x1"), "Cat-Photo-x1");
        assert_eq!(stored_name("../../etc/passwd", "x1"), "passwd-x1");
        assert_eq!(stored_name("C:\\Users\\me\\cat.png", "x1"), "cat-x1");
        assert_eq!(stored_name("猫 写真.jpg", "x1"), "猫-写真-x1");
        assert_eq!(stored_name(".htaccess", "x1"), "file-x1");
        assert_eq!(stored_name("", "x1"), "file-x1");
        assert_eq!(stored_name("!!!.gif", "x1"), "file-x1");
        assert_eq!(stored_name("archive.tar.gz", "x1"), "archive-tar-x1");
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary() {
        // Three bytes per character, so the cut lands mid-character unless
        // it is done by character.
        let name = stored_name(&"猫".repeat(200), "x1");
        let stem = name.strip_suffix("-x1").unwrap();
        assert!(stem.len() <= MAX_NAME_BYTES, "{} bytes", stem.len());
        assert_eq!(stem.len(), 99, "33 whole characters");
        assert!(stem.chars().all(|c| c == '猫'));
    }
}
