//! Who a request is charged to: the client key, and where its address is read.

use std::net::{IpAddr, SocketAddr};

/// What a client is bucketed by.
///
/// A prefix rather than an address, for the reason in the module docs. Held as
/// bytes so v4 and v6 share one key type without either being widened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientKey {
    /// A full IPv4 address.
    V4([u8; 4]),
    /// The first 64 bits of an IPv6 address — one subscriber's allocation.
    V6Prefix([u8; 8]),
}

impl ClientKey {
    pub fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => ClientKey::V4(v4.octets()),
            IpAddr::V6(v6) => {
                // A v4-mapped v6 address is a v4 client arriving over a v6
                // socket; bucketing it as a /64 would give every such client
                // the same key as every other.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return ClientKey::V4(v4.octets());
                }
                let o = v6.octets();
                ClientKey::V6Prefix(o[..8].try_into().expect("16-byte address"))
            }
        }
    }

    /// A short, non-identifying digest for logs.
    ///
    /// The raw address is never logged. This service observes the read pattern
    /// of every wallet in one place; pairing that with an IP would create a
    /// correlation that does not otherwise exist.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        match self {
            ClientKey::V4(b) => h.update(b),
            ClientKey::V6Prefix(b) => h.update(b),
        }
        hex::encode(&h.finalize()[..6])
    }
}

/// Which entry of a forwarding header to believe.
///
/// This is a security choice, not a formatting one. `X-Forwarded-For` is a list
/// that each hop **appends** to, so the entries a caller sent are on the left
/// and the one the nearest trusted proxy added is on the right. Reading the
/// left end means reading whatever the caller wrote there, which lets anyone
/// pick their own rate-limit bucket — and mint an unbounded number of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderPosition {
    /// The last entry: the address the nearest trusted proxy observed.
    ///
    /// The default, and the safe reading of `X-Forwarded-For`, which
    /// Cloudflare appends the visitor's address to.
    ///
    /// Behind Cloudflare the better header is `CF-Connecting-IP`, which
    /// Cloudflare *overwrites* rather than appends — so it carries one entry
    /// and cannot be spoofed through. That holds only while the origin is
    /// unreachable except through Cloudflare.
    #[default]
    Rightmost,
    /// The first entry. Only correct when every hop in the chain is trusted.
    Leftmost,
}

/// Where the client's address is read from.
///
/// Configured, never guessed. Trusting a forwarding header when nothing sets it
/// lets any caller pick their own bucket; trusting the socket when a proxy *is*
/// in front buckets the whole internet into one.
#[derive(Debug, Clone, Default)]
pub struct TrustedHeader {
    pub name: Option<String>,
    pub position: HeaderPosition,
}

impl TrustedHeader {
    /// The socket peer only — a bare process with nothing in front.
    pub fn peer() -> Self {
        Self::default()
    }
}

/// The client key for a request.
///
/// Falls back to the socket peer when the configured header is absent or
/// unusable, which is what a bare `cargo run` needs.
pub fn client_key(
    headers: &axum::http::HeaderMap,
    peer: SocketAddr,
    trusted: &TrustedHeader,
) -> ClientKey {
    let from_header = trusted.name.as_deref().and_then(|name| {
        let raw = headers.get(name)?.to_str().ok()?;
        let mut entries = raw.split(',').map(str::trim).filter(|s| !s.is_empty());
        match trusted.position {
            HeaderPosition::Leftmost => entries.next(),
            HeaderPosition::Rightmost => entries.next_back(),
        }
        .and_then(|s| s.parse::<IpAddr>().ok())
    });
    ClientKey::from_ip(from_header.unwrap_or_else(|| peer.ip()))
}
