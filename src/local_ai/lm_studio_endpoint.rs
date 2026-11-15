//! Chat routes whose transports connect only to loopback.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

#[cfg(not(windows))]
use ureq::Agent;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
#[cfg(not(windows))]
use ureq::unversioned::transport::DefaultConnector;
use ureq::unversioned::transport::NextTimeout;
use ureq::{config::Config, http::Uri};

use super::{Activity, Connection, Error, connection_config};

const INVALID_ENDPOINT: &str =
    "Enter an LM Studio HTTP or HTTPS endpoint on localhost or a numeric loopback address.";
const INVALID_ANTHROPIC: &str =
    "Enter an Anthropic Messages HTTP or HTTPS URL ending in /v1/messages on loopback.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LmStudioEndpoint {
    origin: String,
    native: bool,
    exact_chat_path: Option<String>,
}

impl LmStudioEndpoint {
    pub(crate) fn from_destination(destination: &str) -> Option<Self> {
        let native = has_path_suffix(destination.trim_end_matches('/'), "/api/v1/chat");
        let endpoint = Self::parse(destination, native).ok()?;
        (endpoint.destination() == destination).then_some(endpoint)
    }

    pub(crate) fn parse(endpoint: &str, native: bool) -> Result<Self, String> {
        Self::parse_with_path(endpoint, native, INVALID_ENDPOINT, |path| {
            parse_chat_path(path, native)
        })
    }

    pub(crate) fn parse_anthropic(endpoint: &str) -> Result<Self, String> {
        Self::parse_with_path(endpoint, false, INVALID_ANTHROPIC, |path| {
            if !safe_chat_path(path) {
                return None;
            }
            let path = format!("/{path}");
            has_path_suffix(&path, "/v1/messages").then_some(Some(path))
        })
    }

    fn parse_with_path(
        endpoint: &str,
        native: bool,
        invalid: &'static str,
        parse_path: impl FnOnce(&str) -> Option<Option<String>>,
    ) -> Result<Self, String> {
        let endpoint = endpoint.trim();
        let (scheme, authority, path) = raw_endpoint_parts(endpoint).ok_or(invalid)?;
        let exact_chat_path = parse_path(path).ok_or(invalid)?;
        // A URL parser accepts alternate numeric forms such as 127.1 and octal.
        // Check the written host before normalizing it.
        let host = if authority.starts_with('[') {
            authority
                .split_once(']')
                .map(|(host, _)| &authority[..=host.len()])
        } else {
            Some(authority.split(':').next().unwrap_or_default())
        };
        let host = host.ok_or(invalid)?;
        if authority.ends_with(':')
            || (!endpoint.contains("://") && (host == authority || exact_chat_path.is_some()))
            || (!host.eq_ignore_ascii_case("localhost") && loopback_ip(host).is_none())
        {
            return Err(invalid.into());
        }
        let target = Self::from_origin(scheme, authority, native).ok_or(invalid)?;
        // Saved numeric HTTP profiles have always displayed their explicit port.
        let mut target = target
            .http_address()
            .map_or(target, |address| Self::from_address(address, native));
        target.exact_chat_path = exact_chat_path;
        Ok(target)
    }

    pub(super) fn from_address(address: SocketAddr, native: bool) -> Self {
        Self {
            origin: format!("http://{address}"),
            native,
            exact_chat_path: None,
        }
    }

    pub(super) fn from_origin(scheme: &str, authority: &str, native: bool) -> Option<Self> {
        let origin = url::Url::parse(&format!("{scheme}://{authority}")).ok()?;
        let host = origin.host_str()?;
        if (loopback_ip(host).is_none() && host != "localhost") || origin.port() == Some(0) {
            return None;
        }
        Some(Self {
            origin: origin.origin().ascii_serialization(),
            native,
            exact_chat_path: None,
        })
    }

    pub(crate) fn http_address(&self) -> Option<SocketAddr> {
        let origin = self.origin.strip_prefix("http://")?;
        if let Ok(address) = origin.parse() {
            return Some(address);
        }
        loopback_ip(origin).map(|address| SocketAddr::new(address, 80))
    }

    pub(crate) fn is_https(&self) -> bool {
        self.origin.starts_with("https://")
    }

    pub(crate) fn canonical_http_address(&self) -> Option<SocketAddr> {
        if self.exact_chat_path.is_some() {
            return None;
        }
        self.http_address()
    }

    pub(crate) fn supports_metadata(&self) -> bool {
        self.completion_path().trim_end_matches('/') == canonical_chat_path(self.native)
    }

    pub(super) fn uses_native_protocol(&self) -> bool {
        self.native
    }

    pub(super) fn completion_path(&self) -> &str {
        self.exact_chat_path
            .as_deref()
            .unwrap_or(canonical_chat_path(self.native))
    }

    pub(crate) fn destination(&self) -> String {
        format!("{}{}", self.origin, self.completion_path())
    }

    pub(super) fn connection(&self, timeout: Duration) -> Connection {
        self.connect(connection_config(timeout), Arc::new(Activity::default()))
            .expect("new connection activity is active")
    }

    pub(super) fn connect(
        &self,
        config: Config,
        activity: Arc<Activity>,
    ) -> Result<Connection, Error> {
        #[cfg(windows)]
        let agent = super::windows_transport::agent(config, LoopbackResolver);
        #[cfg(not(windows))]
        let agent = Agent::with_parts(config, DefaultConnector::default(), LoopbackResolver);
        Connection::attach(self.origin.clone(), agent, activity)
    }
}

fn canonical_chat_path(native: bool) -> &'static str {
    if native {
        "/api/v1/chat"
    } else {
        "/v1/chat/completions"
    }
}

fn parse_chat_path(path: &str, native: bool) -> Option<Option<String>> {
    let root = path.trim_end_matches('/');
    if matches!(root, "" | "v1") || native && root == "api/v1" {
        return Some(None);
    }
    if !safe_chat_path(root) {
        return None;
    }
    let full_path = format!("/{path}");
    let trimmed = full_path.trim_end_matches('/');
    let full_path = if !native && has_path_suffix(trimmed, "/v1") {
        format!("{trimmed}/chat/completions")
    } else if !path.ends_with("//")
        && has_path_suffix(
            trimmed,
            if native {
                "/api/v1/chat"
            } else {
                "/chat/completions"
            },
        )
    {
        full_path
    } else {
        return None;
    };
    Some((full_path != canonical_chat_path(native)).then_some(full_path))
}

fn safe_chat_path(path: &str) -> bool {
    path.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'~' | b'-')
    }) && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn has_path_suffix(path: &str, suffix: &str) -> bool {
    path.get(path.len().saturating_sub(suffix.len())..)
        .is_some_and(|last| last.eq_ignore_ascii_case(suffix))
}

pub(super) fn endpoint_parts(endpoint: &str) -> Option<(&'static str, &str, &str)> {
    raw_endpoint_parts(endpoint)
        .map(|(scheme, authority, path)| (scheme, authority, path.trim_end_matches('/')))
}

fn raw_endpoint_parts(endpoint: &str) -> Option<(&'static str, &str, &str)> {
    if endpoint.len() > 2048
        || endpoint
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '\\' | '?' | '#'))
    {
        return None;
    }
    let (scheme, rest) = if let Some(rest) = strip_ascii_prefix(endpoint, "https://") {
        ("https", rest)
    } else {
        (
            "http",
            strip_ascii_prefix(endpoint, "http://").unwrap_or(endpoint),
        )
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    authority
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'[' | b']'))
        .then_some((scheme, authority, path))
}

pub(super) fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let (start, rest) = value.split_at_checked(prefix.len())?;
    start.eq_ignore_ascii_case(prefix).then_some(rest)
}

fn loopback_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().ok().filter(IpAddr::is_loopback)
}

/// Keep localhost local without changing the host name presented to TLS.
#[derive(Debug)]
struct LoopbackResolver;

impl Resolver for LoopbackResolver {
    fn resolve(
        &self,
        uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().ok_or(ureq::Error::HostNotFound)?;
        let default_port = match uri.scheme_str() {
            Some("http") => 80,
            Some("https") => 443,
            _ => return Err(ureq::Error::HostNotFound),
        };
        let port = uri.port_u16().unwrap_or(default_port);
        let mut addresses = self.empty();
        if host == "localhost" {
            addresses.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
            addresses.push(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port));
        } else if let Some(ip) = loopback_ip(host) {
            addresses.push(SocketAddr::new(ip, port));
        } else {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addresses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_anthropic_preserves_only_an_unambiguous_exact_messages_route() {
        for (input, destination) in [
            (
                "HTTP://LOCALHOST:1234/Proxy/V1/Messages",
                "http://localhost:1234/Proxy/V1/Messages",
            ),
            (
                "http://127.0.0.2/v1/messages",
                "http://127.0.0.2:80/v1/messages",
            ),
            (
                "https://[0:0:0:0:0:0:0:1]:8443/v1/messages",
                "https://[::1]:8443/v1/messages",
            ),
        ] {
            let endpoint = LmStudioEndpoint::parse_anthropic(input).unwrap();
            assert_eq!(endpoint.destination(), destination);
            assert!(!endpoint.supports_metadata());
        }
        for input in [
            "localhost:1234/v1/messages",
            "http://localhost./v1/messages",
            "http://127.1:1234/v1/messages",
            "https://[::ffff:127.0.0.1]:1234/v1/messages",
            "https://fixture.test/v1/messages",
            "http://user@localhost:1234/v1/messages",
            "http://localhost:0/v1/messages",
            "http://localhost:1234/v1/messages?token=unsafe",
            "http://localhost:1234/v1/messages#alternate",
            "http://localhost:1234/first/../v1/messages",
            "http://localhost:1234/%2e/v1/messages",
            "http://localhost:1234/first//v1/messages",
            "http://localhost:1234/v1/messages/",
            "http://localhost:1234/v1",
            "http://localhost:1234/api/v1/chat",
        ] {
            assert!(LmStudioEndpoint::parse_anthropic(input).is_err(), "{input}");
        }
    }

    #[test]
    fn lm_studio_endpoint_accepts_only_canonical_loopback_chat_routes() {
        for (input, native, canonical) in [
            ("127.0.0.1:80", true, "http://127.0.0.1:80/api/v1/chat"),
            (
                "127.0.0.1:1234/v1",
                true,
                "http://127.0.0.1:1234/api/v1/chat",
            ),
            (
                " HTTP://LOCALHOST:1234/api/v1/ ",
                true,
                "http://localhost:1234/api/v1/chat",
            ),
            (
                "http://localhost:1234/v1/",
                false,
                "http://localhost:1234/v1/chat/completions",
            ),
            (
                "https://localhost/v1/chat/completions",
                false,
                "https://localhost/v1/chat/completions",
            ),
            (
                "https://[0:0:0:0:0:0:0:1]:1234/api/v1/chat",
                true,
                "https://[::1]:1234/api/v1/chat",
            ),
        ] {
            assert_eq!(
                LmStudioEndpoint::parse(input, native)
                    .unwrap()
                    .destination(),
                canonical
            );
        }
        for input in [
            "http://localhost:",
            "https://localhost:0/v1",
            "https://remote.example/v1",
            "http://localhost.example/v1",
            "http://localhost./v1",
            "http://user@localhost:1234/v1",
            "http://localhost:1234/?token=one",
            "http://localhost:1234/v1#fragment",
            "http://localhost:1234/path/../v1",
            "http://localhost:1234\\v1",
            "http://127.1:1234/v1",
            "http://0177.0.0.1:1234/v1",
            "http://0x7f000001:1234/v1",
            "http://[::ffff:127.0.0.1]:1234/v1",
            "ftp://localhost:1234/v1",
        ] {
            assert!(LmStudioEndpoint::parse(input, true).is_err(), "{input}");
            assert!(LmStudioEndpoint::parse(input, false).is_err(), "{input}");
        }
        assert!(
            LmStudioEndpoint::parse("http://localhost:1234/v1/chat/completions", true).is_err()
        );
        assert!(LmStudioEndpoint::parse("http://localhost:1234/api/v1/chat", false).is_err());
    }

    #[test]
    fn lm_studio_exact_chat_paths_preserve_case_slash_and_prefix() {
        for (native, raw, expected, metadata) in [
            (false, "/V1///", "/V1/chat/completions", false),
            (false, "/tenant/V1/", "/tenant/V1/chat/completions", false),
            (
                false,
                "/v1/chat/completions/",
                "/v1/chat/completions/",
                true,
            ),
            (true, "/api/v1/chat/", "/api/v1/chat/", true),
            (true, "/tenant/API/v1/Chat/", "/tenant/API/v1/Chat/", false),
        ] {
            let origin = "http://127.0.0.1:1234";
            let endpoint = LmStudioEndpoint::parse(&format!("{origin}{raw}"), native).unwrap();
            assert_eq!(endpoint.destination(), format!("{origin}{expected}"));
            assert_eq!(endpoint.supports_metadata(), metadata);
            assert!(endpoint.canonical_http_address().is_none());
            assert_eq!(
                LmStudioEndpoint::from_destination(&endpoint.destination()),
                Some(endpoint)
            );
        }
        for path in [
            "/tenant//V1",
            "/tenant/%2e%2e/V1",
            "/tenant/../V1",
            "/tenant/./V1",
            "/tenant/chat/completions//",
            "/tenant/chat/completions?forward=true",
            "/tenant/chat/completions#alternate",
        ] {
            assert!(
                LmStudioEndpoint::parse(&format!("http://127.0.0.1:1234{path}"), false).is_err(),
                "{path}"
            );
        }
    }
}
