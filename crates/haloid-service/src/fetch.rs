//! Fetching an upload from a URL without becoming a way into the network
//! the server sits on.
//!
//! A URL upload makes the server send a request on a stranger's behalf, so
//! the obvious targets — the cloud metadata endpoint, the admin panel on
//! localhost, the database on the private network — must be unreachable
//! through it, whatever the URL says and wherever it redirects:
//!
//! - only `http` and `https`, and no credentials in the URL;
//! - the host is resolved here, and refused unless *every* address it
//!   resolves to is public — so a name with one public and one private
//!   address cannot be used to pick the private one;
//! - the connection then goes to the address that was checked, pinned, so
//!   a second DNS answer cannot swap in another (DNS rebinding);
//! - redirects are followed by hand, each hop checked the same way, at
//!   most five;
//! - no proxy from the environment, no cookies, a plain user agent, a
//!   timeout, and the caller's size limit enforced while streaming.

use haloid_core::DomainError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use url::Url;

pub const MAX_REDIRECTS: usize = 5;
pub const TIMEOUT: Duration = Duration::from_secs(30);

fn rejected(reason: impl Into<String>) -> DomainError {
    DomainError::UrlFetchRejected(reason.into())
}

/// Whether an address is on the public internet: not loopback, private,
/// link-local, shared, multicast, documentation, reserved or unspecified.
/// An IPv4 address inside IPv6 (mapped, NAT64, 6to4) is judged as IPv4.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_v4(v6) {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10, deprecated
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0)) // discard 100::/64
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local() // 169.254/16, the metadata endpoint included
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_multicast()
        || v4.is_documentation()
        || a == 0 // "this network"
        || (a == 100 && (b & 0xc0) == 64) // carrier-grade NAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (b & 0xfe) == 18) // benchmarking 198.18/15
        || a >= 240) // reserved
}

/// The IPv4 address an IPv6 address stands for, if it is one of the forms
/// that carry one: v4-mapped `::ffff:a.b.c.d`, v4-compatible `::a.b.c.d`,
/// NAT64 `64:ff9b::/96`, or 6to4 `2002:aabb:ccdd::/48`.
fn embedded_v4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = v6.segments();
    let tail = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if s[..6] == [0; 6] && !(s[6] == 0 && s[7] <= 1) {
        return Some(tail); // v4-compatible, not :: or ::1
    }
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        return Some(tail);
    }
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    None
}

/// What may be reached. Production allows only public addresses; the test
/// suites, whose mock servers listen on loopback, allow loopback too —
/// nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Public,
    PublicAndLoopback,
}

impl Reach {
    fn allows(self, ip: IpAddr) -> bool {
        is_public(ip) || (self == Self::PublicAndLoopback && ip.to_canonical().is_loopback())
    }
}

/// A body on its way in, and the name it came by.
pub struct Fetched {
    pub reader: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
    pub filename: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Fetcher {
    reach: Reach,
    user_agent: String,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new(Reach::Public)
    }
}

impl Fetcher {
    pub fn new(reach: Reach) -> Self {
        Self {
            reach,
            user_agent: format!("Haloid/{} (+url upload)", env!("CARGO_PKG_VERSION")),
        }
    }

    /// The address to connect to for `url`, once every address it names
    /// has passed.
    async fn checked_address(&self, url: &Url) -> Result<SocketAddr, DomainError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(rejected("only http and https URLs can be uploaded from"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(rejected("URLs with credentials in them are refused"));
        }
        let port = url
            .port_or_known_default()
            .ok_or_else(|| rejected("the URL has no port"))?;
        let addresses: Vec<SocketAddr> = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
            Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
            Some(url::Host::Domain(name)) => tokio::net::lookup_host((name, port))
                .await
                .map_err(|_| rejected("the host does not resolve"))?
                .collect(),
            None => return Err(rejected("the URL has no host")),
        };
        if addresses.is_empty() {
            return Err(rejected("the host does not resolve"));
        }
        if let Some(bad) = addresses.iter().find(|a| !self.reach.allows(a.ip())) {
            tracing::info!(url = %url, address = %bad.ip(), "url upload refused: not a public address");
            return Err(rejected(
                "the URL points at an address that cannot be fetched from",
            ));
        }
        Ok(addresses[0])
    }

    /// Start fetching `url`. The body is the caller's to read, within its
    /// own size limit; a `Content-Length` over `limit` is refused first.
    pub async fn fetch(&self, url: &str, limit: u64) -> Result<Fetched, DomainError> {
        let mut current = Url::parse(url.trim()).map_err(|_| rejected("not a valid URL"))?;
        for _ in 0..=MAX_REDIRECTS {
            let address = self.checked_address(&current).await?;
            let host = current.host_str().unwrap_or_default().to_string();
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(TIMEOUT)
                .connect_timeout(Duration::from_secs(10))
                .user_agent(&self.user_agent)
                // The name resolves to the address just checked, and no other.
                .resolve(&host, address)
                .build()
                .map_err(|e| DomainError::Internal(e.into()))?;
            let response = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| rejected("the URL could not be fetched"))?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| rejected("a redirect without a destination"))?;
                current = current
                    .join(location)
                    .map_err(|_| rejected("a redirect to an invalid URL"))?;
                continue;
            }
            if !status.is_success() {
                return Err(rejected(format!("the URL answered {}", status.as_u16())));
            }
            if let Some(length) = response.content_length()
                && length > limit
            {
                return Err(DomainError::SizeLimit {
                    size: length,
                    limit,
                });
            }
            let filename = current
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .map(percent_decode);
            use futures::TryStreamExt;
            let stream = response.bytes_stream().map_err(std::io::Error::other);
            return Ok(Fetched {
                reader: Box::new(tokio_util::io::StreamReader::new(stream)),
                filename,
            });
        }
        Err(rejected(format!("more than {MAX_REDIRECTS} redirects")))
    }
}

fn percent_decode(segment: &str) -> String {
    url::form_urlencoded::parse(format!("x={segment}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| segment.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "198.18.0.1",
            "192.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:0101::1",
            "100::1",
        ] {
            assert!(
                !is_public(blocked.parse().unwrap()),
                "{blocked} should be refused"
            );
        }
        for allowed in [
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(is_public(allowed.parse().unwrap()), "{allowed} should pass");
        }
    }

    #[tokio::test]
    async fn literal_private_addresses_and_odd_urls_are_refused_without_a_request() {
        let fetcher = Fetcher::default();
        for url in [
            "http://127.0.0.1/a.png",
            "http://[::1]/a.png",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::ffff:10.0.0.1]/",
            "http://0x7f000001/",
            "http://2130706433/",
            "ftp://example.com/a.png",
            "file:///etc/passwd",
            "http://user:pass@example.com/a.png",
            "not a url",
            "http://localhost/a.png",
        ] {
            let error = fetcher
                .fetch(url, 1024)
                .await
                .err()
                .unwrap_or_else(|| panic!("{url} was fetched"));
            assert!(
                matches!(error, DomainError::UrlFetchRejected(_)),
                "{url}: {error:?}"
            );
        }
    }
}
