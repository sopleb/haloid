//! IP addresses in `VARBINARY(16)` columns.
//!
//! Four bytes for IPv4, sixteen for IPv6. An IPv4-mapped IPv6 address
//! (`::ffff:192.0.2.1`, which a dual-stack listener reports for IPv4 clients)
//! is stored as the IPv4 address it is, so one client never appears as two.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub(crate) fn to_bytes(ip: IpAddr) -> Vec<u8> {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

/// Anything that is not four or sixteen bytes reads as absent rather than
/// failing the row: an address is diagnostic, never load-bearing.
pub(crate) fn from_bytes(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes)
            .ok()
            .map(|b| IpAddr::V4(Ipv4Addr::from(b))),
        16 => <[u8; 16]>::try_from(bytes)
            .ok()
            .map(|b| IpAddr::V6(Ipv6Addr::from(b))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_families_round_trip() {
        for text in ["192.0.2.1", "2001:db8::1", "::1", "0.0.0.0"] {
            let ip: IpAddr = text.parse().unwrap();
            assert_eq!(from_bytes(&to_bytes(ip)), Some(ip), "{text}");
        }
    }

    #[test]
    fn an_ipv4_mapped_address_is_stored_as_ipv4() {
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(to_bytes(mapped), vec![192, 0, 2, 1]);
        assert_eq!(
            from_bytes(&to_bytes(mapped)),
            Some("192.0.2.1".parse().unwrap())
        );
    }

    #[test]
    fn odd_lengths_read_as_absent() {
        for bytes in [&[][..], &[1, 2, 3], &[0; 5], &[0; 17]] {
            assert_eq!(from_bytes(bytes), None, "{bytes:?}");
        }
    }
}
