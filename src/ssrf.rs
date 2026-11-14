//! SSRF guard, ported from upstream `shared/utils/ssrfGuard.js`.
use std::net::IpAddr;

/// False for loopback, private, link-local, CGNAT, metadata, multicast and IPv4-in-IPv6 forms of those.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || o[0] == 0
                || o[0] >= 224
                || (o[0] == 100 && (64..=127).contains(&o[1])))
        }
        IpAddr::V6(ip) => {
            let g = ip.segments();
            let low = std::net::Ipv4Addr::new((g[6] >> 8) as u8, g[6] as u8, (g[7] >> 8) as u8, g[7] as u8);
            // IPv4-mapped, NAT64 64:ff9b::/96 and deprecated IPv4-compatible all embed an IPv4 address.
            if ip.to_ipv4_mapped().is_some()
                || (g[0] == 0x64 && g[1] == 0xff9b && g[2..6] == [0; 4])
                || (g[..6] == [0; 6] && !ip.is_loopback() && !ip.is_unspecified())
            {
                return public_address(low.into());
            }
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (g[0] & 0xfe00) == 0xfc00
                || (g[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn ip_literal(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

/// Upstream `dashboardGuard.isLocalRequest`: loopback TCP peer, no forwarding headers,
/// and a loopback Origin when one is sent. Missing peer info (tests) is never local.
pub fn is_local_request(peer: Option<std::net::SocketAddr>, headers: &axum::http::HeaderMap) -> bool {
    let loopback = |ip: IpAddr| ip.is_loopback() || matches!(ip, IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()));
    if !peer.is_some_and(|p| loopback(p.ip())) {
        return false;
    }
    if headers.contains_key("x-forwarded-for") || headers.contains_key("x-real-ip") {
        return false;
    }
    match headers.get("origin").map(|o| o.to_str().ok().and_then(|o| reqwest::Url::parse(o).ok())) {
        None => true,
        Some(Some(u)) => u.host_str().is_some_and(|h| h == "localhost" || ip_literal(h).is_some_and(loopback)),
        Some(None) => false,
    }
}

/// Literal check: scheme, blocked hostnames and IP literals. No DNS.
pub fn literal_ok(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    if let Some(ip) = ip_literal(host) {
        return public_address(ip);
    }
    let d = host.trim_end_matches('.').to_ascii_lowercase();
    !(matches!(d.as_str(), "localhost" | "ip6-localhost" | "ip6-loopback" | "")
        || [".internal", ".local", ".localhost"].iter().any(|s| d.ends_with(s)))
}

/// Literal check plus DNS: rejects hostnames that resolve to internal addresses.
/// ponytail: DNS rebinding between this check and the fetch is not pinned; pin with
/// `ClientBuilder::resolve_to_addrs` (as validate_node does) if this becomes a concern.
pub async fn url_is_public(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else { return false };
    if !literal_ok(&url) {
        return false;
    }
    let Some(d) = url.host_str().filter(|h| ip_literal(h).is_none()) else { return true };
    let port = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((d, port)).await {
        Ok(addrs) => addrs.into_iter().all(|a| public_address(a.ip())),
        Err(_) => true, // resolution failure: let the fetch fail with its own error, like upstream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_request_rules() {
        let lo: std::net::SocketAddr = "127.0.0.1:5".parse().unwrap();
        let pub_: std::net::SocketAddr = "203.0.113.1:5".parse().unwrap();
        let h = |k: &str, v: &str| { let mut m = axum::http::HeaderMap::new(); m.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap()); m };
        assert!(is_local_request(Some(lo), &Default::default()));
        assert!(is_local_request(Some(lo), &h("origin", "http://localhost:20128")));
        assert!(!is_local_request(Some(lo), &h("origin", "https://evil.example")));
        assert!(!is_local_request(Some(lo), &h("x-forwarded-for", "1.2.3.4")));
        assert!(!is_local_request(Some(pub_), &Default::default()));
        assert!(!is_local_request(None, &Default::default()));
    }

    #[tokio::test]
    async fn blocks_internal_forms() {
        for u in [
            "http://localhost/", "http://localhost./", "http://127.0.0.1/", "http://10.1.2.3/",
            "http://169.254.169.254/latest", "http://100.64.0.1/", "http://[::1]/",
            "http://[::ffff:127.0.0.1]/", "http://[::ffff:7f00:1]/", "http://[64:ff9b::a00:1]/",
            "http://[fe80::1]/", "http://[fd00::1]/", "http://foo.internal/", "file:///etc/passwd",
            "http://0.0.0.0/", "http://172.16.5.5/",
        ] {
            assert!(!url_is_public(u).await, "{u} should be blocked");
        }
        assert!(url_is_public("https://1.1.1.1/").await);
        assert!(url_is_public("http://[2606:4700::1111]/").await);
    }
}
