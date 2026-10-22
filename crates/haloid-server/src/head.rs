//! Link-preview tags, injected into the SPA's `index.html` for the public
//! pages: `/f/{id}`, `/a/{id}` and `/u/{username}`.
//!
//! Chat apps and search engines read `<head>` tags, not rendered pages, so
//! the server fills them in: title, Open Graph, Twitter card, canonical URL,
//! JSON-LD, and a preload of the image the page will show. The SPA still
//! fetches its own data and never depends on these existing.
//!
//! Tags are only ever made for what a guest may see — a crawler is a guest —
//! so a private or unapproved file never gets a preview. Every value is
//! HTML-escaped: titles and descriptions are whatever users typed.

use haloid_core::auth::Actor;
use haloid_core::media::MediaKind;
use haloid_service::app::App;
use std::fmt::Write;

/// The marker in `app.html` that the tags replace.
pub const MARKER: &str = "<!--%haloid.head%-->";

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// JSON for a `<script type="application/ld+json">`: `<` escaped too, so a
/// value containing `</script>` cannot close the element.
fn script_json(value: &serde_json::Value) -> String {
    value.to_string().replace('<', "\\u003c")
}

#[derive(Default)]
struct Tags {
    out: String,
}

impl Tags {
    fn meta(&mut self, property: &str, content: &str) {
        let _ = write!(
            self.out,
            r#"<meta property="{}" content="{}" />"#,
            escape(property),
            escape(content)
        );
    }

    fn name(&mut self, name: &str, content: &str) {
        let _ = write!(
            self.out,
            r#"<meta name="{}" content="{}" />"#,
            escape(name),
            escape(content)
        );
    }

    fn raw(&mut self, html: &str) {
        self.out.push_str(html);
    }
}

/// The page's `<head>` additions, or `None` when there is nothing a guest
/// may see there (the SPA then shows its own not-found).
pub async fn for_path(app: &App, path: &str) -> Option<String> {
    let mut parts = path.trim_matches('/').split('/');
    let (kind, id) = (parts.next()?, parts.next()?);
    let settings = app.settings.get();
    let site = escape(&settings.site.name);
    let now = chrono::Utc::now();
    let mut tags = Tags::default();

    match kind {
        "f" => {
            let shown = app.uploads.view(Actor::Guest, id, now).await.ok()?;
            let file = &shown.file;
            let title = file
                .title
                .clone()
                .unwrap_or_else(|| settings.site.name.clone());
            let url = shown.urls.viewer.clone();
            let image = shown
                .urls
                .derivatives
                .get("medium")
                .or_else(|| shown.urls.derivatives.get("frame"))
                .or_else(|| shown.urls.derivatives.get("image"))
                .cloned();
            let _ = write!(tags.out, "<title>{} · {site}</title>", escape(&title));
            tags.raw(&format!(
                r#"<link rel="canonical" href="{}" />"#,
                escape(&url)
            ));
            tags.meta("og:site_name", &settings.site.name);
            tags.meta("og:title", &title);
            tags.meta("og:url", &url);
            if let Some(description) = &file.description {
                tags.meta("og:description", description);
                tags.name("description", description);
            }
            match file.kind {
                MediaKind::Image => {
                    tags.meta("og:type", "website");
                    tags.name("twitter:card", "summary_large_image");
                }
                MediaKind::Video => {
                    tags.meta("og:type", "video.other");
                    if let Some(video) = shown.urls.derivatives.get("image") {
                        tags.meta("og:video", video);
                        tags.meta("og:video:type", &file.mime);
                    }
                    tags.name("twitter:card", "player");
                }
                MediaKind::Audio => {
                    tags.meta("og:type", "music.song");
                    if let Some(audio) = shown.urls.derivatives.get("image") {
                        tags.meta("og:audio", audio);
                    }
                    tags.name("twitter:card", "summary");
                }
            }
            if let Some(image) = &image
                && file.kind != MediaKind::Audio
            {
                tags.meta("og:image", image);
                if file.kind == MediaKind::Image {
                    tags.meta("og:image:width", &file.width.to_string());
                    tags.meta("og:image:height", &file.height.to_string());
                    tags.meta("og:image:type", &file.mime);
                }
                tags.name("twitter:image", image);
                // The viewer shows this image first: fetch it with the HTML.
                tags.raw(&format!(
                    r#"<link rel="preload" as="image" href="{}" />"#,
                    escape(image)
                ));
            }
            let schema = match file.kind {
                MediaKind::Image => "ImageObject",
                MediaKind::Video => "VideoObject",
                MediaKind::Audio => "AudioObject",
            };
            let ld = serde_json::json!({
                "@context": "https://schema.org",
                "@type": schema,
                "name": title,
                "description": file.description,
                "contentUrl": shown.urls.derivatives.get("image"),
                "thumbnailUrl": shown.urls.derivatives.get("thumb"),
                "uploadDate": file.created_at.to_rfc3339(),
                "width": (file.kind != MediaKind::Audio).then_some(file.width),
                "height": (file.kind != MediaKind::Audio).then_some(file.height),
            });
            let _ = write!(
                tags.out,
                r#"<script type="application/ld+json">{}</script>"#,
                script_json(&ld)
            );
        }
        "a" => {
            let album = app.library.album(Actor::Guest, id).await.ok()?;
            let url = format!("{}/a/{}", settings.site.base_url, album.id_encoded);
            let _ = write!(tags.out, "<title>{} · {site}</title>", escape(&album.name));
            tags.raw(&format!(
                r#"<link rel="canonical" href="{}" />"#,
                escape(&url)
            ));
            tags.meta("og:site_name", &settings.site.name);
            tags.meta("og:type", "website");
            tags.meta("og:title", &album.name);
            tags.meta("og:url", &url);
            if let Some(description) = &album.description {
                tags.meta("og:description", description);
            }
            tags.name("twitter:card", "summary");
        }
        "u" => {
            let user = app.library.user(Actor::Guest, id).await.ok()?;
            let name = user
                .display_name
                .clone()
                .unwrap_or_else(|| user.username.clone());
            let url = format!("{}/u/{}", settings.site.base_url, user.username);
            let _ = write!(tags.out, "<title>{} · {site}</title>", escape(&name));
            tags.raw(&format!(
                r#"<link rel="canonical" href="{}" />"#,
                escape(&url)
            ));
            tags.meta("og:site_name", &settings.site.name);
            tags.meta("og:type", "profile");
            tags.meta("og:title", &name);
            tags.meta("og:url", &url);
            tags.meta("profile:username", &user.username);
            tags.name("twitter:card", "summary");
        }
        _ => return None,
    }
    Some(tags.out)
}

/// `index.html` with the tags in place of the marker.
pub fn inject(index: &str, tags: &str) -> String {
    index.replacen(MARKER, tags, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_cannot_break_out_of_an_attribute_or_a_script() {
        assert_eq!(
            escape(r#""><script>x</script>"#),
            "&quot;&gt;&lt;script&gt;x&lt;/script&gt;"
        );
        let ld = script_json(&serde_json::json!({"name": "</script><script>alert(1)"}));
        assert!(!ld.contains("</script>"), "{ld}");
    }

    #[test]
    fn injection_replaces_only_the_marker() {
        let page = format!("<head>{MARKER}<meta charset=\"utf-8\"></head>");
        assert_eq!(
            inject(&page, "<title>x</title>"),
            "<head><title>x</title><meta charset=\"utf-8\"></head>"
        );
        assert_eq!(
            inject("<head></head>", "<title>x</title>"),
            "<head></head>",
            "no marker, no change"
        );
    }
}
