//! The domain name a relay gives for itself. Discovery only learns addresses,
//! so a relay puts its first `--name` that is a domain among its `RInfoReply`
//! capabilities. A client uses it only if it resolves to the address it
//! dialed: anyone can claim a name, but only its owner can point it somewhere.

use std::net::{IpAddr, SocketAddr};

/// Prefix of the capability that carries the name.
pub(crate) const CAP_PREFIX: &str = "name:";
/// Longest host name.
const MAX_HOST: usize = 253;

/// True when `name` is a `host:port` whose host is a public-looking domain:
/// dotted, plain ASCII, not an address and not an onion name.
#[must_use]
pub fn is_domain_name(name: &str) -> bool {
    let Some((host, port)) = name.rsplit_once(':') else {
        return false;
    };
    let Some((_, last)) = host.rsplit_once('.') else {
        return false;
    };
    port.parse::<u16>()
        .is_ok_and(|number| number != 0 && number.to_string() == port)
        && host.len() <= MAX_HOST
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        // A numeric last label is an address, however it is spelled.
        && !last.bytes().all(|byte| byte.is_ascii_digit())
        && !last.eq_ignore_ascii_case("onion")
}

/// The domain name among a relay's capabilities.
pub(crate) fn claimed(caps: &[String]) -> Option<&str> {
    caps.iter()
        .find_map(|cap| cap.strip_prefix(CAP_PREFIX))
        .filter(|name| is_domain_name(name))
}

/// True when `resolved` includes the address the relay was dialed at.
fn points_at(resolved: impl IntoIterator<Item = SocketAddr>, addr: SocketAddr) -> bool {
    resolved.into_iter().any(|found| {
        found.port() == addr.port() && found.ip().to_canonical() == IpAddr::to_canonical(&addr.ip())
    })
}

/// Check a claimed name against `addr`. `None` when it cannot be resolved just
/// now, which says nothing either way.
pub(crate) async fn check(name: &str, addr: SocketAddr) -> Option<bool> {
    let found = tokio::net::lookup_host(name).await.ok()?;
    Some(points_at(found, addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_domains_with_a_port_are_names() {
        for good in ["relay.example.org:42425", "a-1.b.io:1"] {
            assert!(is_domain_name(good), "{good}");
        }
        for bad in [
            "relay.example.org",
            "relay.example.org:0",
            "relay.example.org:+1",
            "localhost:1",
            "203.0.113.7:42425",
            "[::1]:1",
            "abc.onion:1",
            ".example.org:1",
            "a..org:1",
            "a b.org:1",
            "a.org#key:1",
            "a.\u{1b}[2J:1",
        ] {
            assert!(!is_domain_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_claim_is_read_only_when_it_is_a_domain() {
        let caps = |name: &str| vec!["listen".to_string(), format!("name:{name}")];
        assert_eq!(claimed(&caps("r.example:1")), Some("r.example:1"));
        assert_eq!(claimed(&caps("10.0.0.1:1")), None);
        assert_eq!(claimed(&["listen".to_string()]), None);
    }

    #[test]
    fn a_name_must_point_at_the_dialed_address_and_port() {
        let at = |text: &str| text.parse::<SocketAddr>().unwrap();
        let dialed = at("93.184.216.34:42425");
        assert!(points_at(
            [at("1.1.1.1:42425"), at("[::ffff:93.184.216.34]:42425")],
            dialed
        ));
        assert!(!points_at([at("1.1.1.1:42425")], dialed));
        assert!(!points_at([at("93.184.216.34:1")], dialed));
    }
}
