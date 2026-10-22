//! Who is calling: an API key, a browser session, or a guest.
//!
//! `Authorization: Bearer hld_…` for API clients; the `__Host-haloid_session`
//! cookie for the browser; neither is a guest. A browser's cookie is sent
//! automatically by any page that can make it send a request, so every
//! unsafe request that relies on one must also prove it came from our own
//! page: an `X-Requested-With` header (which a cross-site form cannot set)
//! and an `Origin` matching the site. Bearer requests need neither — no
//! other site can make a browser attach a key.

use axum::http::{HeaderMap, Method, header};
use chrono::{DateTime, Utc};
use haloid_core::DomainError;
use haloid_core::auth::Actor;
use haloid_service::app::App;

/// The session cookie on an HTTPS (or localhost) site: `__Host-` makes the
/// browser refuse it unless it is `Secure`, host-only and path `/`.
pub const SESSION_COOKIE: &str = "__Host-haloid_session";
/// The same cookie on a plain-HTTP site, where a browser would silently drop
/// a `__Host-` or `Secure` one — a self-hosted instance on a LAN.
pub const PLAIN_SESSION_COOKIE: &str = "haloid_session";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub actor: Actor,
    pub via_cookie: bool,
    /// The session behind the cookie, for logging out.
    pub session: Option<haloid_core::ids::SessionId>,
}

/// Whether the site can use the hardened cookie: it is served over HTTPS,
/// or from localhost, which browsers treat as secure.
pub fn secure_site(base_url: &str) -> bool {
    base_url.starts_with("https://")
        || ["http://localhost", "http://127.0.0.1", "http://[::1]"]
            .iter()
            .any(|local| {
                base_url == *local
                    || base_url.starts_with(&format!("{local}:"))
                    || base_url.starts_with(&format!("{local}/"))
            })
}

/// The one session cookie name this site sets and accepts.
pub fn session_cookie_name(base_url: &str) -> &'static str {
    if secure_site(base_url) {
        SESSION_COOKIE
    } else {
        PLAIN_SESSION_COOKIE
    }
}

/// The `Set-Cookie` value that starts a session, or (with `None`) ends one.
pub fn session_cookie(base_url: &str, token: Option<&str>, max_age_secs: u64) -> String {
    let (name, secure) = if secure_site(base_url) {
        (SESSION_COOKIE, "; Secure")
    } else {
        (PLAIN_SESSION_COOKIE, "")
    };
    match token {
        Some(token) => format!(
            "{name}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
        ),
        None => format!("{name}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
    }
}

/// A password album's unlock cookie: set on the album's own path family is
/// not possible (files live at `/f/`), so it is site-wide, `HttpOnly` and
/// `SameSite=Lax`, and holds only a signed grant, never the password.
pub fn album_cookie(base_url: &str, album: &str, grant: &str, max_age_secs: u64) -> String {
    let secure = if base_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{}={grant}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}",
        haloid_service::album_locks::cookie_name(album)
    )
}

/// Every album grant the request carries, as `(album id, grant)`.
pub fn album_grants(headers: &HeaderMap) -> Vec<(String, String)> {
    let prefix = haloid_service::album_locks::cookie_name("");
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter_map(|(k, v)| {
            k.strip_prefix(prefix.as_str())
                .map(|album| (album.to_string(), v.to_string()))
        })
        .collect()
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub async fn identify(
    app: &App,
    headers: &HeaderMap,
    now: DateTime<Utc>,
) -> Result<Caller, DomainError> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(key) = bearer {
        let actor = app.auth.api_key_actor(key, now).await?;
        return Ok(Caller {
            actor,
            via_cookie: false,
            session: None,
        });
    }
    // Only the cookie this site would itself set. On an HTTPS site an
    // unprefixed `haloid_session` could have been planted by a sibling
    // subdomain — exactly what `__Host-` exists to rule out — so it is
    // never honoured there.
    let name = session_cookie_name(&app.settings.get().site.base_url);
    let token = cookie(headers, name);
    if let Some(token) = token
        && let Some((actor, session)) = app.auth.session_actor(token, now).await?
    {
        return Ok(Caller {
            actor,
            via_cookie: true,
            session: Some(session.id),
        });
    }
    // An unknown or expired cookie is no identity at all: a guest.
    Ok(Caller {
        actor: Actor::Guest,
        via_cookie: false,
        session: None,
    })
}

/// The CSRF rule for a cookie-authenticated unsafe request.
pub fn check_csrf(
    caller: &Caller,
    method: &Method,
    headers: &HeaderMap,
    base_url: &str,
) -> Result<(), DomainError> {
    if !caller.via_cookie || method.is_safe() {
        return Ok(());
    }
    same_origin(headers, base_url)
}

/// That a request came from our own page: `X-Requested-With`, which a
/// cross-site form cannot set, and our `Origin`. Required of the login and
/// registration forms whatever the caller, so another site cannot log a
/// visitor into an account of its choosing.
pub fn same_origin(headers: &HeaderMap, base_url: &str) -> Result<(), DomainError> {
    let requested_with = headers.contains_key("x-requested-with");
    let origin_matches = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|origin| origin == origin_of(base_url));
    if requested_with && origin_matches {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// `https://img.example.com:8443/some/path` → `https://img.example.com:8443`.
fn origin_of(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    match url[after_scheme..].find('/') {
        Some(slash) => &url[..after_scheme + slash],
        None => url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use haloid_core::auth::Role;
    use haloid_core::ids::UserId;

    fn user() -> Caller {
        Caller {
            actor: Actor::User {
                id: UserId(1),
                role: Role::User,
            },
            via_cookie: true,
            session: None,
        }
    }

    #[test]
    fn the_hardened_cookie_needs_a_secure_site() {
        assert!(secure_site("https://img.example.com"));
        assert!(secure_site("http://localhost:8080"));
        assert!(secure_site("http://127.0.0.1"));
        assert!(!secure_site("http://192.168.1.20:8080"));
        assert!(!secure_site("http://localhost.evil.example"));

        let https = session_cookie("https://img.example.com", Some("t"), 60);
        assert!(https.starts_with("__Host-haloid_session=t;"));
        assert!(
            https.contains("; Secure") && https.contains("HttpOnly") && https.contains("Path=/")
        );
        let lan = session_cookie("http://192.168.1.20", Some("t"), 60);
        assert!(lan.starts_with("haloid_session=t;") && !lan.contains("Secure"));
        assert!(session_cookie("https://x.example", None, 0).contains("Max-Age=0"));
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn cookies_are_found_among_others() {
        let h = headers(&[("cookie", "theme=dark; __Host-haloid_session=abc123; x=y")]);
        assert_eq!(cookie(&h, SESSION_COOKIE), Some("abc123"));
        assert_eq!(cookie(&h, "missing"), None);
    }

    #[test]
    fn cookie_writes_need_our_header_and_our_origin() {
        let base = "https://img.example.com";
        let post = Method::POST;
        let ok = headers(&[
            ("x-requested-with", "XMLHttpRequest"),
            ("origin", "https://img.example.com"),
        ]);
        assert!(check_csrf(&user(), &post, &ok, base).is_ok());

        for bad in [
            headers(&[("origin", "https://img.example.com")]),
            headers(&[("x-requested-with", "XMLHttpRequest")]),
            headers(&[
                ("x-requested-with", "XMLHttpRequest"),
                ("origin", "https://evil.example"),
            ]),
            headers(&[
                ("x-requested-with", "XMLHttpRequest"),
                ("origin", "https://img.example.com.evil.example"),
            ]),
        ] {
            assert!(check_csrf(&user(), &post, &bad, base).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reads_and_non_cookie_callers_are_exempt() {
        let none = HeaderMap::new();
        assert!(check_csrf(&user(), &Method::GET, &none, "https://x").is_ok());
        let bearer = Caller {
            via_cookie: false,
            ..user()
        };
        assert!(check_csrf(&bearer, &Method::POST, &none, "https://x").is_ok());
    }

    #[test]
    fn origins_drop_the_path() {
        assert_eq!(
            origin_of("https://img.example.com"),
            "https://img.example.com"
        );
        assert_eq!(
            origin_of("http://localhost:8080/base"),
            "http://localhost:8080"
        );
    }
}
