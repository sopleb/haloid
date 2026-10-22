//! The client's real address behind a reverse proxy.
//!
//! Everything that is per address — rate limits, the uploader's IP, the
//! audit log — reads the connection's peer address. Behind a proxy that is
//! the proxy, for everyone. When the peer is one of `HALOID_TRUSTED_PROXIES`,
//! the client is the rightmost `X-Forwarded-For` entry that is not itself
//! a trusted proxy; anything left of it could have been written by the
//! client and is ignored. From any other peer the header is ignored
//! entirely, so a client cannot claim an address by sending it.

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

pub fn client_address(peer: IpAddr, forwarded: Option<&str>, trusted: &[IpNet]) -> IpAddr {
    let is_trusted = |ip: IpAddr| trusted.iter().any(|net| net.contains(&ip.to_canonical()));
    if !is_trusted(peer) {
        return peer;
    }
    let Some(header) = forwarded else { return peer };
    let mut client = peer;
    for hop in header.rsplit(',') {
        match hop.trim().parse::<IpAddr>() {
            Ok(ip) => {
                client = ip;
                if !is_trusted(ip) {
                    break;
                }
            }
            // A malformed entry: stop at the last good one.
            Err(_) => break,
        }
    }
    client
}

/// Middleware: rewrite the connection's address to the client's, so every
/// handler downstream sees the real one.
pub async fn real_client(trusted: Arc<Vec<IpNet>>, mut request: Request, next: Next) -> Response {
    if let Some(ConnectInfo(peer)) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied()
    {
        let forwarded = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok());
        let client = client_address(peer.ip(), forwarded, &trusted);
        if client != peer.ip() {
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(client, peer.port())));
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|n| n.parse().unwrap()).collect()
    }

    #[test]
    fn only_a_trusted_peer_is_believed_and_only_from_the_right() {
        let trusted = nets(&["10.0.0.0/8", "127.0.0.1/32"]);
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // Not from a proxy: the header is the client's to forge, so ignored.
        assert_eq!(
            client_address(ip("203.0.113.9"), Some("1.2.3.4"), &trusted),
            ip("203.0.113.9")
        );
        // From the proxy: the address it saw.
        assert_eq!(
            client_address(ip("10.0.0.2"), Some("198.51.100.7"), &trusted),
            ip("198.51.100.7")
        );
        // A forged entry on the left is skipped; the proxy chain on the
        // right is walked past.
        assert_eq!(
            client_address(
                ip("127.0.0.1"),
                Some("6.6.6.6, 198.51.100.7, 10.0.0.5"),
                &trusted
            ),
            ip("198.51.100.7")
        );
        // No header, or nothing usable: the peer.
        assert_eq!(
            client_address(ip("10.0.0.2"), None, &trusted),
            ip("10.0.0.2")
        );
        assert_eq!(
            client_address(ip("10.0.0.2"), Some("junk"), &trusted),
            ip("10.0.0.2")
        );
        // A v4-mapped peer is matched as IPv4.
        assert_eq!(
            client_address(ip("::ffff:10.0.0.2"), Some("198.51.100.7"), &trusted),
            ip("198.51.100.7")
        );
        // Trusting nothing believes nothing.
        assert_eq!(
            client_address(ip("10.0.0.2"), Some("198.51.100.7"), &[]),
            ip("10.0.0.2")
        );
    }
}
