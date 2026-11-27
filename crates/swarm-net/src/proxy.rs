//! Outbound TCP, directly or through a SOCKS5 proxy (spec 11.10, M9).
//!
//! Every outbound TCP connection Swarmgit makes goes through [`connect`]. When
//! `SWARM_PROXY` is set, nothing is dialed directly and no name is resolved
//! here: the proxy gets the name and resolves it at its end.

use std::fmt;
use std::io;
use std::net::SocketAddr;

use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

/// Environment variable naming the proxy (`config.yaml`: `proxy`).
pub const PROXY_ENV: &str = "SWARM_PROXY";

/// Where to connect: an address, or a name that only a resolver understands.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// A literal socket address.
    Ip(SocketAddr),
    /// A DNS or onion name.
    Name {
        /// Host name.
        host: String,
        /// TCP port.
        port: u16,
    },
}

impl Endpoint {
    /// The socket address, when this is a literal one.
    #[must_use]
    pub fn socket(&self) -> Option<SocketAddr> {
        match self {
            Self::Ip(addr) => Some(*addr),
            Self::Name { .. } => None,
        }
    }

    /// True for a Tor onion service name.
    #[must_use]
    pub fn is_onion(&self) -> bool {
        matches!(self, Self::Name { host, .. } if host.ends_with(".onion"))
    }

    /// Parse `ip:port`, `[ipv6]:port` or `name:port`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if let Ok(addr) = value.parse::<SocketAddr>() {
            return Some(Self::Ip(addr));
        }
        let (host, port) = value.rsplit_once(':')?;
        let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
        let valid = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
        valid.then(|| Self::Name {
            host: host.to_string(),
            port,
        })
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip(addr) => write!(f, "{addr}"),
            Self::Name { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

/// A SOCKS5 proxy.
#[derive(Clone, PartialEq, Eq)]
pub struct Proxy {
    /// The proxy's own `host:port`.
    pub server: String,
    /// Username and password, when the proxy wants them.
    pub auth: Option<(String, String)>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The password never reaches a log.
        write!(f, "socks5://{}", self.server)
    }
}

impl Proxy {
    /// Parse `socks5://[user:password@]host:port`. `socks5h://` and a bare
    /// `host:port` mean the same: names are always resolved by the proxy.
    pub fn parse(value: &str) -> io::Result<Self> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "proxy must be socks5://[user:password@]host:port",
            )
        };
        let value = value.trim().trim_end_matches('/');
        let rest = match value.split_once("://") {
            Some(("socks5" | "socks5h", rest)) => rest,
            Some(_) => return Err(bad()),
            None => value,
        };
        let (auth, server) = match rest.rsplit_once('@') {
            Some((auth, server)) => {
                let (user, password) = auth.split_once(':').ok_or_else(bad)?;
                if user.is_empty() || password.is_empty() {
                    return Err(bad());
                }
                (Some((user.to_string(), password.to_string())), server)
            }
            None => (None, rest),
        };
        Endpoint::parse(server).ok_or_else(bad)?;
        Ok(Self {
            server: server.to_string(),
            auth,
        })
    }

    /// The configured proxy. A value that does not parse is an error, so a
    /// typo never turns into direct connections.
    pub fn from_env() -> io::Result<Option<Self>> {
        match std::env::var(PROXY_ENV) {
            Ok(value) if !value.trim().is_empty() => Self::parse(&value).map(Some),
            _ => Ok(None),
        }
    }

    /// `CONNECT` to `to` through this proxy.
    pub async fn connect(&self, to: &Endpoint) -> io::Result<TcpStream> {
        let server = self.server.as_str();
        // A literal address is sent as an address, a name as a name.
        let (host, port) = match to {
            Endpoint::Ip(addr) => (addr.ip().to_string(), addr.port()),
            Endpoint::Name { host, port } => (host.clone(), *port),
        };
        let target = (host.as_str(), port);
        let stream = match &self.auth {
            Some((user, password)) => {
                Socks5Stream::connect_with_password(server, target, user, password).await
            }
            None => Socks5Stream::connect(server, target).await,
        };
        stream
            .map(Socks5Stream::into_inner)
            .map_err(|err| match err {
                tokio_socks::Error::Io(err) => err,
                other => {
                    io::Error::new(io::ErrorKind::ConnectionRefused, format!("proxy: {other}"))
                }
            })
    }
}

/// Open a TCP connection to `to`: through the proxy when one is configured,
/// directly otherwise. An onion name without a proxy is refused: looking it
/// up would tell the local resolver which service is wanted.
pub async fn connect(to: &Endpoint) -> io::Result<TcpStream> {
    match Proxy::from_env()? {
        Some(proxy) => proxy.connect(to).await,
        None if to.is_onion() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an onion address needs the proxy setting",
        )),
        None => match to {
            Endpoint::Ip(addr) => TcpStream::connect(addr).await,
            Endpoint::Name { host, port } => TcpStream::connect((host.as_str(), *port)).await,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proxy_urls_and_rejects_others() {
        let plain = Proxy::parse("socks5://127.0.0.1:9050").unwrap();
        assert_eq!(plain.server, "127.0.0.1:9050");
        assert_eq!(Proxy::parse("socks5h://127.0.0.1:9050/").unwrap(), plain);
        assert_eq!(Proxy::parse("127.0.0.1:9050").unwrap(), plain);
        let auth = Proxy::parse("socks5://me:s3cret@tor:9050").unwrap();
        assert_eq!(auth.auth, Some(("me".into(), "s3cret".into())));
        assert!(!format!("{auth:?}").contains("s3cret"));
        for bad in ["http://a:1", "socks5://nohost", "socks5://u@h:1", ""] {
            assert!(Proxy::parse(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn an_onion_name_is_never_dialed_without_a_proxy() {
        // Tests run without SWARM_PROXY set.
        let onion = Endpoint::parse("abc.onion:42424").unwrap();
        let err = connect(&onion).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn endpoints_parse_both_shapes() {
        assert_eq!(
            Endpoint::parse("[::1]:5").unwrap().socket(),
            Some("[::1]:5".parse().unwrap())
        );
        let onion = Endpoint::parse("abc.onion:42424").unwrap();
        assert!(onion.is_onion() && onion.socket().is_none());
        assert_eq!(onion.to_string(), "abc.onion:42424");
        for bad in ["nohost", "a b:1", "host:0", ":1", "host:99999"] {
            assert!(Endpoint::parse(bad).is_none(), "{bad}");
        }
    }
}
