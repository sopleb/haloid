//! Assembling a legacy request from its query string, form fields and
//! headers — upstream's rules, including the odd ones.
//!
//! Fields come from the query string and the body, the body winning (PHP's
//! `$_REQUEST`), except the API key, where the `X-API-Key` header beats any
//! `key` field. Then the aliases, in an order that is observable:
//!
//! * `image` is a synonym for `source`, **and its mere presence flips the
//!   whole response envelope to the ImgBB shape**. That flag is captured
//!   here, at parse time, and carried on [`LegacyRequest`]; normalising
//!   `image` into `source` early would silently lose it. It counts only as a
//!   text field: a *file* uploaded under `image` is not a source upstream,
//!   so it is "Empty upload source" there and here.
//! * `name` is a synonym for `title`.
//! * `album_id` is a public (encoded) id while `category_id` is a plain
//!   integer. Inconsistent upstream, kept.
//! * `use_file_date` is admin-only upstream and silently ignored for anyone
//!   else; we do not implement it, so it is ignored for everyone.

use haloid_core::DomainError;

/// The parts of an HTTP request the legacy API looks at.
#[derive(Debug, Clone, Default)]
pub struct RawRequest {
    /// `1` in `/api/1/upload`.
    pub version: String,
    /// `upload` in `/api/1/upload`.
    pub action: String,
    pub is_get: bool,
    pub header_key: Option<String>,
    pub query: Vec<(String, String)>,
    /// Text form fields, multipart or urlencoded.
    pub body: Vec<(String, String)>,
    /// Whether a file field named `source` arrived.
    pub has_source_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    /// The bare image URL, or the bare error message.
    Txt,
    /// A 302 to the viewer, or the bare error message.
    Redirect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The uploaded `source` file.
    File,
    Base64(String),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRequest {
    pub format: Format,
    pub key: String,
    pub action: String,
    /// Whether to answer in ImgBB's envelope.
    pub imgbb: bool,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub album_id: Option<String>,
    pub category_id: Option<u32>,
    pub nsfw: Option<bool>,
    pub width: Option<u32>,
    pub expiration: Option<String>,
    raw_source: Option<String>,
    has_source_file: bool,
    is_get: bool,
}

impl RawRequest {
    /// The body's value if it has one, else the query's; the last of
    /// repeated fields, as PHP keeps.
    fn get(&self, name: &str) -> Option<&str> {
        fn last<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
            pairs
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }
        last(&self.body, name).or_else(|| last(&self.query, name))
    }

    fn present(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn text(&self, name: &str) -> Option<String> {
        self.get(name)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }
}

/// The `format` field, read first because every later error is rendered in
/// it. Anything unrecognised is JSON, as upstream's `default:` case.
pub fn format(raw: &RawRequest) -> Format {
    match raw.get("format").map(str::trim) {
        Some("txt") => Format::Txt,
        Some("redirect") => Format::Redirect,
        _ => Format::Json,
    }
}

/// Validate what can be validated before the key is checked, in upstream's
/// order: a missing version, action or key, then an unsupported version.
pub fn parse(raw: &RawRequest) -> Result<LegacyRequest, DomainError> {
    let key = raw
        .header_key
        .clone()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| raw.text("key"))
        .unwrap_or_default();
    if raw.version.is_empty() || raw.action.is_empty() || key.is_empty() {
        return Err(DomainError::MissingApiKey);
    }
    if raw.version != "1" {
        return Err(DomainError::UnsupportedApiVersion);
    }

    let flag = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => true,
        // Upstream treats anything but "1" as not NSFW.
        _ => false,
    };
    Ok(LegacyRequest {
        format: format(raw),
        key,
        action: raw.action.clone(),
        imgbb: raw.present("image"),
        title: raw.text("title").or_else(|| raw.text("name")),
        description: raw.text("description"),
        tags: raw
            .get("tags")
            .map(|tags| {
                tags.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        album_id: raw.text("album_id"),
        category_id: raw.text("category_id").and_then(|c| c.parse().ok()),
        nsfw: raw.get("nsfw").map(flag),
        width: raw
            .text("width")
            .and_then(|w| w.parse().ok())
            .filter(|&w| w > 0),
        expiration: raw.text("expiration"),
        raw_source: raw
            .get("source")
            .or_else(|| raw.get("image"))
            .map(str::to_string),
        has_source_file: raw.has_source_file,
        is_get: raw.is_get,
    })
}

impl LegacyRequest {
    /// Checked after authentication, as upstream does: a bad key outranks a
    /// bad action.
    pub fn check_action(&self) -> Result<(), DomainError> {
        if self.action == "upload" {
            Ok(())
        } else {
            Err(DomainError::UnknownAction)
        }
    }

    /// Where the bytes come from: the `source` file, else a `source` or
    /// `image` text field, which is a URL or base64. Base64 must come by
    /// POST; upstream refuses it in a query string.
    pub fn source(&self) -> Result<Source, DomainError> {
        if self.has_source_file {
            return Ok(Source::File);
        }
        let Some(text) = self.raw_source.as_deref() else {
            return Err(DomainError::EmptySource);
        };
        let trimmed = text.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Ok(Source::Url(trimmed.to_string()));
        }
        if self.is_get {
            return Err(DomainError::Base64ViaGet);
        }
        if trimmed.is_empty() {
            return Err(DomainError::EmptySource);
        }
        Ok(Source::Base64(text.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(query: &[(&str, &str)], body: &[(&str, &str)]) -> RawRequest {
        let pairs = |p: &[(&str, &str)]| {
            p.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        RawRequest {
            version: "1".into(),
            action: "upload".into(),
            is_get: false,
            header_key: None,
            query: pairs(query),
            body: pairs(body),
            has_source_file: false,
        }
    }

    #[test]
    fn the_body_beats_the_query_but_the_header_beats_both_for_the_key() {
        let mut r = raw(
            &[("key", "query"), ("title", "q")],
            &[("key", "body"), ("title", "b")],
        );
        let parsed = parse(&r).unwrap();
        assert_eq!(parsed.key, "body");
        assert_eq!(parsed.title.as_deref(), Some("b"));

        r.header_key = Some("header".into());
        assert_eq!(parse(&r).unwrap().key, "header");
    }

    #[test]
    fn the_image_field_is_a_source_and_flips_the_envelope() {
        let parsed = parse(&raw(&[], &[("key", "k"), ("image", "aGVsbG8=")])).unwrap();
        assert!(parsed.imgbb);
        assert_eq!(parsed.source().unwrap(), Source::Base64("aGVsbG8=".into()));

        // Present at all — even empty — and the envelope flips.
        let empty = parse(&raw(&[], &[("key", "k"), ("image", "")])).unwrap();
        assert!(empty.imgbb);

        // `source` wins over `image` as the bytes, but the flag still holds.
        let both = parse(&raw(
            &[],
            &[("key", "k"), ("source", "c291cmNl"), ("image", "aW1hZ2U=")],
        ))
        .unwrap();
        assert!(both.imgbb);
        assert_eq!(both.source().unwrap(), Source::Base64("c291cmNl".into()));

        let plain = parse(&raw(&[], &[("key", "k"), ("source", "aGVsbG8=")])).unwrap();
        assert!(!plain.imgbb);
    }

    #[test]
    fn a_file_under_image_is_no_source() {
        // Upstream reads files from `source` only (captured).
        let mut r = raw(&[], &[("key", "k")]);
        r.has_source_file = false;
        let parsed = parse(&r).unwrap();
        assert!(!parsed.imgbb);
        assert!(matches!(parsed.source(), Err(DomainError::EmptySource)));
    }

    #[test]
    fn errors_come_in_upstreams_order() {
        let mut r = raw(&[], &[]);
        assert!(matches!(parse(&r), Err(DomainError::MissingApiKey)));
        r.body.push(("key".into(), "k".into()));
        r.version = "2".into();
        assert!(matches!(parse(&r), Err(DomainError::UnsupportedApiVersion)));
        r.version = "1".into();
        r.action = "delete".into();
        let parsed = parse(&r).expect("the action is checked later, after the key");
        assert!(matches!(
            parsed.check_action(),
            Err(DomainError::UnknownAction)
        ));
    }

    #[test]
    fn base64_must_be_posted_but_urls_need_not_be() {
        let mut r = raw(&[("key", "k"), ("source", "aGVsbG8=")], &[]);
        r.is_get = true;
        assert!(matches!(
            parse(&r).unwrap().source(),
            Err(DomainError::Base64ViaGet)
        ));

        let mut url = raw(
            &[("key", "k"), ("source", "https://example.com/a.jpg")],
            &[],
        );
        url.is_get = true;
        assert_eq!(
            parse(&url).unwrap().source().unwrap(),
            Source::Url("https://example.com/a.jpg".into())
        );
    }

    #[test]
    fn aliases_and_parameter_parsing() {
        let parsed = parse(&raw(
            &[],
            &[
                ("key", "k"),
                ("name", "From name"),
                ("tags", "cats, black and white,,"),
                ("album_id", "Ab3dE5fG7h"),
                ("category_id", "7"),
                ("nsfw", "on"),
                ("width", "800"),
                ("expiration", "PT5M"),
                ("format", "txt"),
            ],
        ))
        .unwrap();
        assert_eq!(parsed.title.as_deref(), Some("From name"), "name is title");
        assert_eq!(parsed.tags, vec!["cats", "black and white"]);
        assert_eq!(parsed.album_id.as_deref(), Some("Ab3dE5fG7h"));
        assert_eq!(parsed.category_id, Some(7));
        assert_eq!(parsed.nsfw, Some(true), "browsers send on");
        assert_eq!(parsed.width, Some(800));
        assert_eq!(parsed.expiration.as_deref(), Some("PT5M"));
        assert_eq!(parsed.format, Format::Txt);

        let title_wins = parse(&raw(&[], &[("key", "k"), ("title", "T"), ("name", "N")])).unwrap();
        assert_eq!(title_wins.title.as_deref(), Some("T"));

        let junk = parse(&raw(
            &[],
            &[
                ("key", "k"),
                ("width", "-5"),
                ("category_id", "x"),
                ("nsfw", "maybe"),
            ],
        ))
        .unwrap();
        assert_eq!(
            (junk.width, junk.category_id, junk.nsfw),
            (None, None, Some(false))
        );
    }

    #[test]
    fn unknown_formats_are_json() {
        for (value, expected) in [
            ("json", Format::Json),
            ("txt", Format::Txt),
            ("redirect", Format::Redirect),
            ("xml", Format::Json),
            ("", Format::Json),
        ] {
            assert_eq!(
                format(&raw(&[("format", value)], &[])),
                expected,
                "{value:?}"
            );
        }
    }
}
