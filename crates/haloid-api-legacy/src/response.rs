//! The legacy response body, field for field.
//!
//! Field order is the contract — clients and the golden tests both see it —
//! so the object is built by hand in exactly the order a live upstream
//! instance emits (`fixtures/golden/upstream/`), never derived from a
//! struct.
//!
//! Typing follows one documented rule: the numeric-looking fields upstream
//! historically sent as strings (`STRING_FIELDS`) are strings here, by
//! decision, though current upstream sends integers. Everything else keeps
//! upstream's own type — including its quirk that `display_width` is a
//! string when it comes from a setting and an integer when it comes from the
//! image.

use chrono::{DateTime, Utc};
use haloid_core::media::{Derivative, MediaKind};
use haloid_core::settings::MediumDimension;
use haloid_core::storage::StorageKind;
use haloid_service::upload::UploadedFile;
use serde_json::{Map, Value, json};

/// What the body needs beyond the file itself.
pub struct Context<'a> {
    pub base_url: &'a str,
    pub thumb_edge: u32,
    pub medium_edge: u32,
    pub medium_dimension: MediumDimension,
    pub now: DateTime<Utc>,
}

/// A numeric field sent as a JSON string.
fn s(value: impl ToString) -> Value {
    Value::String(value.to_string())
}

fn date(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Upstream's `seoUrlfy`, closely enough: letters and digits kept (case
/// included), every run of anything else one `-`.
fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(ch);
        } else {
            dash = true;
        }
    }
    out
}

/// Upstream's `truncate(title, 28)`: cut to fit with `...` on the end.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit - 3).collect();
    format!("{}...", kept.trim())
}

/// `htmlspecialchars` with `ENT_QUOTES | ENT_HTML5`.
fn html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Upstream's `time_elapsed_string`, for `how_long_ago`.
fn how_long_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    let unit = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one} ago")
        } else {
            format!("{n} {one}s ago")
        }
    };
    match secs {
        0..60 => "moments ago".to_string(),
        60..3600 => unit(secs / 60, "minute"),
        3600..86_400 => unit(secs / 3600, "hour"),
        86_400..2_592_000 => unit(secs / 86_400, "day"),
        2_592_000..31_536_000 => unit(secs / 2_592_000, "month"),
        _ => unit(secs / 31_536_000, "year"),
    }
}

/// `width / height`: an integer when it divides exactly, as PHP's division
/// gives, otherwise a float.
fn ratio(width: u32, height: u32) -> Value {
    if height == 0 {
        return json!(1);
    }
    if width.is_multiple_of(height) {
        json!(width / height)
    } else {
        json!(f64::from(width) / f64::from(height))
    }
}

/// The five-null object a missing derivative serialises as.
fn absent() -> Value {
    json!({"filename": null, "name": null, "mime": null, "extension": null, "url": null})
}

/// The legacy image object.
pub fn image_object(uploaded: &UploadedFile, ctx: &Context<'_>) -> Value {
    let file = &uploaded.file;
    let chain = file.chain.legacy();
    let title = file.title.clone().unwrap_or_else(|| {
        // Upstream stores the original file name's stem as the title.
        let original = file.original_filename.as_deref().unwrap_or(&file.name);
        original
            .rsplit_once('.')
            .map_or(original, |(stem, _)| stem)
            .to_string()
    });
    let original_filename = match file.original_filename.as_deref() {
        Some(name) if name.contains('.') => name.to_string(),
        Some(name) => format!("{name}.{}", file.extension),
        None => format!("{}.{}", file.name, file.extension),
    };
    let duration_secs = file.duration_ms.map_or(0, |ms| ms / 1000);
    let (derivative_ext, derivative_mime) = file.kind.derivative_format(&file.extension);
    let url_of = |d: Derivative| uploaded.urls.derivatives.get(d.legacy_key()).cloned();

    let mut o = Map::new();
    o.insert("name".into(), json!(file.name));
    o.insert("extension".into(), json!(file.extension));
    o.insert("size".into(), s(file.size));
    o.insert("width".into(), s(file.width));
    o.insert("height".into(), s(file.height));
    o.insert("date".into(), json!(date(file.created_at)));
    o.insert("date_gmt".into(), json!(date(file.created_at)));
    o.insert("title".into(), json!(title));
    o.insert("description".into(), json!(file.description));
    o.insert("nsfw".into(), s(u8::from(file.nsfw)));
    o.insert("storage_mode".into(), json!(file.storage_mode.as_str()));
    o.insert("checksum".into(), json!(file.checksum));
    // Upstream records a source checksum only when a resize altered the
    // upload — which is exactly when we keep an original. (Comparing the two
    // checksums would be wrong: metadata stripping changes the bytes too.)
    o.insert(
        "source_checksum".into(),
        json!(
            file.source_checksum
                .as_ref()
                .filter(|_| file.chain.contains(Derivative::Original))
        ),
    );
    o.insert("original_filename".into(), json!(original_filename));
    o.insert("original_exifdata".into(), json!(uploaded.exif));
    o.insert("views".into(), s(file.view_count));
    o.insert(
        "category_id".into(),
        json!(file.category_id.map(|c| c.0.to_string())),
    );
    o.insert("chain".into(), json!(chain.bits()));
    o.insert("thumb_size".into(), json!(file.thumb_size.unwrap_or(0)));
    o.insert("medium_size".into(), json!(file.medium_size.unwrap_or(0)));
    o.insert("frame_size".into(), json!(file.frame_size.unwrap_or(0)));
    o.insert(
        "expiration_date_gmt".into(),
        json!(file.expires_at.map(date)),
    );
    o.insert("likes".into(), s(file.like_count));
    o.insert("is_animated".into(), s(u8::from(file.is_animated)));
    o.insert("is_approved".into(), s(u8::from(file.is_approved)));
    o.insert("is_360".into(), s(u8::from(file.is_360)));
    o.insert("duration".into(), s(duration_secs));
    o.insert(
        "type".into(),
        json!(match file.kind {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Image => "image",
        }),
    );
    o.insert("delete_hash".into(), json!(uploaded.delete_hash));
    o.insert(
        "tags".into(),
        Value::Array(
            uploaded
                .tags
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "name_safe_html": html(&t.name),
                        "url_key": t.slug,
                        "url": format!("/tag/{}", t.slug),
                    })
                })
                .collect(),
        ),
    );
    o.insert(
        "tags_string".into(),
        json!(
            uploaded
                .tags
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    let resource = match uploaded.storage_kind {
        Some(StorageKind::S3) => "url",
        _ => "path",
    };
    o.insert("file".into(), json!({"resource": {"type": resource}}));
    o.insert("id_encoded".into(), json!(file.id_encoded.as_str()));
    o.insert(
        "filename".into(),
        json!(format!("{}.{}", file.name, file.extension)),
    );
    o.insert("mime".into(), json!(file.mime));
    o.insert("url".into(), json!(url_of(Derivative::Image)));
    o.insert("ratio".into(), ratio(file.width, file.height));
    o.insert(
        "size_formatted".into(),
        json!(crate::errors::format_bytes(file.size)),
    );

    // Each stored derivative, in upstream's chain order.
    for derivative in [
        Derivative::Frame,
        Derivative::Image,
        Derivative::Medium,
        Derivative::Thumb,
    ] {
        if !chain.contains(derivative) {
            continue;
        }
        let (extension, mime, size) = match derivative {
            Derivative::Image => (file.extension.as_str(), file.mime.as_str(), file.size),
            Derivative::Thumb => (
                derivative_ext,
                derivative_mime,
                file.thumb_size.unwrap_or(0),
            ),
            Derivative::Medium => (
                derivative_ext,
                derivative_mime,
                file.medium_size.unwrap_or(0),
            ),
            _ => (
                derivative_ext,
                derivative_mime,
                file.frame_size.unwrap_or(0),
            ),
        };
        let name = format!("{}{}", file.name, derivative.suffix());
        o.insert(
            derivative.legacy_key().into(),
            json!({
                "filename": format!("{name}.{extension}"),
                "name": name,
                "mime": mime,
                "extension": extension,
                "url": url_of(derivative),
                "size": s(size),
            }),
        );
    }
    let frame_url = url_of(Derivative::Frame).filter(|_| chain.contains(Derivative::Frame));
    o.insert(
        "url_frame".into(),
        json!(frame_url.clone().unwrap_or_default()),
    );
    if !chain.contains(Derivative::Medium) {
        o.insert("medium".into(), absent());
    }
    if !chain.contains(Derivative::Thumb) {
        o.insert("thumb".into(), absent());
    }
    o.insert(
        "duration_time".into(),
        json!(if duration_secs > 0 {
            format!("{:02}:{:02}", duration_secs / 60, duration_secs % 60)
        } else {
            String::new()
        }),
    );

    let base = ctx.base_url;
    let id = file.id_encoded.as_str();
    let slugged = slug(&title);
    let url_viewer = if slugged.is_empty() {
        format!("{base}/f/{id}")
    } else {
        format!("{base}/f/{id}/{slugged}")
    };
    let path_viewer = url_viewer
        .strip_prefix(base)
        .map_or_else(|| url_viewer.clone(), str::to_string);
    o.insert("url_viewer".into(), json!(url_viewer));
    o.insert("path_viewer".into(), json!(path_viewer));
    o.insert("url_short".into(), json!(format!("{base}/f/{id}")));

    // Upstream's display choice: the medium if there is one; else the thumb
    // for a large image; and the frame instead whenever it is smaller.
    let mut display_url = frame_url.clone().or_else(|| url_of(Derivative::Image));
    let mut display_width = json!(file.width);
    let mut display_height = json!(file.height);
    let mut display_size = None;
    if chain.contains(Derivative::Medium) {
        display_url = url_of(Derivative::Medium);
        let image_ratio = f64::from(file.width) / f64::from(file.height.max(1));
        let edge = ctx.medium_edge;
        match ctx.medium_dimension {
            MediumDimension::Width => {
                display_width = s(edge);
                display_height = json!((f64::from(edge) / image_ratio).round() as u64);
            }
            MediumDimension::Height => {
                display_height = s(edge);
                display_width = json!((f64::from(edge) * image_ratio).round() as u64);
            }
            MediumDimension::Longest => {}
        }
        display_size = file.medium_size;
    } else if chain.contains(Derivative::Thumb)
        && file.size > 200_000
        && file.kind == MediaKind::Image
    {
        display_url = url_of(Derivative::Thumb);
        display_width = s(ctx.thumb_edge);
        display_height = s(ctx.thumb_edge);
        display_size = file.thumb_size;
    }
    if let (Some(frame), Some(display)) = (file.frame_size, display_size)
        && frame < display
    {
        display_url = frame_url;
        display_width = json!(file.width);
        display_height = json!(file.height);
    }
    o.insert("display_url".into(), json!(display_url.unwrap_or_default()));
    o.insert("display_width".into(), display_width);
    o.insert("display_height".into(), display_height);
    o.insert(
        "views_label".into(),
        json!(if file.view_count == 1 {
            "view"
        } else {
            "views"
        }),
    );
    o.insert(
        "likes_label".into(),
        json!(if file.like_count == 1 {
            "like"
        } else {
            "likes"
        }),
    );
    o.insert(
        "how_long_ago".into(),
        json!(how_long_ago(file.created_at, ctx.now)),
    );
    o.insert("date_fixed_peer".into(), json!(date(file.created_at)));
    let truncated = truncate(&title, 28);
    o.insert("title_truncated".into(), json!(truncated));
    o.insert("title_truncated_html".into(), json!(html(&truncated)));
    o.insert("is_use_loader".into(), json!(false));
    o.insert("display_title".into(), json!(title));
    o.insert("delete_url".into(), json!(uploaded.urls.delete));

    // An unapproved file's URLs are removed — absent, not blank — except
    // `frame.url`. Upstream's asymmetry, and the contract.
    if !file.is_approved {
        o.shift_remove("url");
        o.shift_remove("display_url");
        for key in ["image", "thumb", "medium"] {
            if let Some(Value::Object(derivative)) = o.get_mut(key) {
                derivative.shift_remove("url");
            }
        }
    }
    Value::Object(o)
}

/// The whole success body. `imgbb` swaps in ImgBB's envelope: a top-level
/// `status`, the payload under `data`, and `data.id`.
pub fn success(image: Value, imgbb: bool) -> Value {
    let mut body = Map::new();
    body.insert("status_code".into(), json!(200));
    if imgbb {
        body.insert("status".into(), json!(200));
    }
    body.insert(
        "success".into(),
        json!({"message": "file uploaded", "code": 200}),
    );
    let mut image = image;
    if imgbb && let Value::Object(map) = &mut image {
        let id = map.get("id_encoded").cloned().unwrap_or(Value::Null);
        map.insert("id".into(), id);
    }
    body.insert(if imgbb { "data" } else { "image" }.into(), image);
    // Appended by upstream's output function, so it comes last.
    body.insert("status_txt".into(), json!("OK"));
    Value::Object(body)
}

pub fn error(message: &str, code: i64) -> Value {
    json!({
        "status_code": 400,
        "error": {"message": message, "code": code},
        "status_txt": "Bad Request",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_slug_truncate_and_escape_as_upstream_does() {
        assert_eq!(slug("Golden title"), "Golden-title");
        assert_eq!(slug("  a / b  "), "a-b");
        assert_eq!(truncate("short", 28), "short");
        let long = "a".repeat(40);
        assert_eq!(truncate(&long, 28), format!("{}...", "a".repeat(25)));
        assert_eq!(
            html("<b>\"Tom & Jerry's\"</b>"),
            "&lt;b&gt;&quot;Tom &amp; Jerry&apos;s&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn ratios_are_integers_when_they_divide() {
        assert_eq!(ratio(40, 20), json!(2));
        assert_eq!(ratio(900, 600), json!(1.5));
        assert_eq!(ratio(30, 30), json!(1));
        assert_eq!(ratio(10, 0), json!(1));
    }

    #[test]
    fn ages_read_like_upstreams() {
        let t = Utc::now();
        assert_eq!(how_long_ago(t, t), "moments ago");
        assert_eq!(
            how_long_ago(t, t + chrono::Duration::minutes(1)),
            "1 minute ago"
        );
        assert_eq!(
            how_long_ago(t, t + chrono::Duration::hours(5)),
            "5 hours ago"
        );
    }

    #[test]
    fn envelopes_are_in_upstream_order() {
        let body = success(json!({"id_encoded": "x"}), false);
        let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["status_code", "success", "image", "status_txt"]);

        let imgbb = success(json!({"id_encoded": "x"}), true);
        let keys: Vec<&String> = imgbb.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["status_code", "status", "success", "data", "status_txt"]
        );
        assert_eq!(imgbb["data"]["id"], "x");

        assert_eq!(
            crate::php_json::to_string(&error("No key provided", 100)),
            include_str!("../../../fixtures/golden/upstream/error-no-key-json.body")
        );
    }
}
