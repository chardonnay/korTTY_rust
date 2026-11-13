use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use toml_edit::{Item, Table};

use super::{allowed, invalid, string, trim_java};

const SCOPE: &str = "rule.servers";
static DECIMAL_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A\p{Nd}\z").unwrap());

/// The effective rules for configured SSH and other remote terminal endpoints.
#[derive(Clone, Debug, Default)]
pub(crate) struct ServerAccess {
    rules: Arc<Vec<Restriction>>,
    lockdown: bool,
}

impl ServerAccess {
    pub(super) fn lockdown() -> Self {
        Self {
            lockdown: true,
            ..Self::default()
        }
    }

    pub(super) fn add_rule(&mut self, table: &Table) -> io::Result<()> {
        allowed(table, SCOPE, &["mode", "hosts"])?;
        let allow = match string(table, SCOPE, "mode")?.map(trim_java) {
            Some(mode) if mode.eq_ignore_ascii_case("allow") => true,
            Some(mode) if mode.eq_ignore_ascii_case("deny") => false,
            _ => return Err(invalid("rule.servers.mode must be allow or deny")),
        };
        let hosts = table
            .get("hosts")
            .and_then(Item::as_array)
            .filter(|hosts| !hosts.is_empty())
            .ok_or_else(|| invalid("rule.servers.hosts must be a non-empty array of patterns"))?;
        let patterns = hosts
            .iter()
            .enumerate()
            .map(|(index, host)| {
                host.as_str()
                    .ok_or("expected a text pattern")
                    .and_then(Pattern::parse)
                    .map_err(|reason| {
                        invalid(format!("rule.servers.hosts #{}: {reason}", index + 1))
                    })
            })
            .collect::<io::Result<_>>()?;
        Arc::make_mut(&mut self.rules).push(Restriction { allow, patterns });
        Ok(())
    }

    pub(crate) fn is_restricted(&self) -> bool {
        self.lockdown || !self.rules.is_empty()
    }

    pub(crate) fn allows(&self, host: &str, port: u16) -> bool {
        if self.lockdown {
            return false;
        }
        if !self.is_restricted() {
            return true;
        }
        let host = Host::parse(host);
        // The Java matcher keeps DNS names and IP literals separate. A native resolver
        // can nevertheless interpret legacy numeric spellings as IPv4 addresses.
        if matches!(&host, Some(Host::Name(name)) if resolver_ipv4_alias(name)) {
            return false;
        }
        self.rules.iter().all(|rule| {
            rule.patterns.iter().any(|pattern| {
                host.as_ref()
                    .is_some_and(|host| pattern.matches(host, port))
            }) == rule.allow
        })
    }

    /// Supply the active jump hosts in order, followed by the final destination.
    pub(crate) fn first_blocked<'a>(
        &self,
        route: impl IntoIterator<Item = (&'a str, u16)>,
    ) -> Option<(&'a str, u16)> {
        route
            .into_iter()
            .find(|&(host, port)| !self.allows(host, port))
    }
}

#[derive(Clone, Debug)]
struct Restriction {
    allow: bool,
    patterns: Vec<Pattern>,
}

#[derive(Clone, Debug)]
struct Pattern {
    host: HostPattern,
    port: Option<u16>,
}

#[derive(Clone, Debug)]
enum HostPattern {
    ExactName(String),
    NameGlob(Regex),
    ExactIp(IpAddr),
    Network(IpAddr, u8),
    Range(IpAddr, IpAddr),
}

impl Pattern {
    fn parse(input: &str) -> Result<Self, &'static str> {
        let input = trim_java(input);
        if let Some(bracketed) = input.strip_prefix('[') {
            let (address, suffix) = bracketed.split_once(']').ok_or("unclosed IPv6 brackets")?;
            let address = parse_ip(trim_java(address))
                .filter(IpAddr::is_ipv6)
                .ok_or("invalid bracketed IPv6 address")?;
            let port = if suffix.is_empty() {
                None
            } else {
                let port = suffix
                    .strip_prefix(':')
                    .ok_or("expected port after IPv6 brackets")?;
                Some(parse_port(port)?)
            };
            return Ok(Self {
                host: HostPattern::ExactIp(address),
                port,
            });
        }
        if let Some((address, prefix)) = input.split_once('/') {
            let address = parse_ip(trim_java(address)).ok_or("invalid CIDR network address")?;
            let prefix = parse_java_integer(prefix)
                .and_then(|prefix| u8::try_from(prefix).ok())
                .filter(|&prefix| prefix <= if address.is_ipv4() { 32 } else { 128 })
                .ok_or("invalid CIDR prefix length")?;
            return Ok(Self {
                host: HostPattern::Network(address, prefix),
                port: None,
            });
        }
        if let Some((low, high)) = input.split_once('-')
            && let Some(low) = parse_ip(trim_java(low))
        {
            let high = parse_ip(strip_brackets(trim_java(high))).ok_or("invalid IP range end")?;
            if low.is_ipv4() != high.is_ipv4() || low > high {
                return Err("invalid IP range bounds");
            }
            return Ok(Self {
                host: HostPattern::Range(low, high),
                port: None,
            });
        }
        let (host, port) = match input.split_once(':') {
            Some((_, rest)) if rest.contains(':') => {
                let address = parse_ip(input).ok_or("invalid IPv6 address")?;
                return Ok(Self {
                    host: HostPattern::ExactIp(address),
                    port: None,
                });
            }
            Some((host, port)) => (host, Some(parse_port(port)?)),
            None => (input, None),
        };
        let host = match parse_ip(host) {
            Some(address) => HostPattern::ExactIp(address),
            None => HostPattern::parse_name(host)?,
        };
        Ok(Self { host, port })
    }

    fn matches(&self, host: &Host, port: u16) -> bool {
        if self.port.is_some_and(|required| required != port) {
            return false;
        }
        match (&self.host, host) {
            (HostPattern::ExactName(expected), Host::Name(host)) => host == expected,
            (HostPattern::NameGlob(glob), Host::Name(host)) => glob.is_match(host),
            (HostPattern::ExactIp(expected), Host::Ip(address)) => address == expected,
            (HostPattern::Network(network, prefix), Host::Ip(address)) => {
                cidr_contains(*network, *prefix, *address)
            }
            (HostPattern::Range(low, high), Host::Ip(address)) => {
                address.is_ipv4() == low.is_ipv4() && low <= address && address <= high
            }
            _ => false,
        }
    }
}

impl HostPattern {
    fn parse_name(host: &str) -> Result<Self, &'static str> {
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"*?._-".contains(&byte))
        {
            return Err("invalid host pattern");
        }
        let host = host.to_ascii_lowercase();
        if !host.contains(['*', '?']) {
            return Ok(Self::ExactName(host));
        }
        let mut expression = String::from(r"\A");
        let mut star = false;
        for character in host.chars() {
            match character {
                '*' if !star => expression.push_str(".*"),
                '*' => {}
                '?' => expression.push('.'),
                '.' => expression.push_str(r"\."),
                _ => expression.push(character),
            }
            star = character == '*';
        }
        expression.push_str(r"\z");
        Regex::new(&expression)
            .map(Self::NameGlob)
            .map_err(|_| "host pattern is too complex")
    }
}

enum Host {
    Name(String),
    Ip(IpAddr),
}

impl Host {
    fn parse(host: &str) -> Option<Self> {
        if host.chars().all(java_whitespace) {
            return None;
        }
        let host = strip_brackets(trim_java(host));
        if let Some(address) = parse_ip(host) {
            return Some(Self::Ip(address));
        }
        // Java's glob wildcards do not match any of its line terminators.
        (!host.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']))
            .then(|| Self::Name(host.to_lowercase()))
    }
}

fn java_whitespace(character: char) -> bool {
    ('\u{1c}'..='\u{1f}').contains(&character)
        || (character.is_whitespace()
            && !matches!(character, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
}

fn parse_ip(input: &str) -> Option<IpAddr> {
    // The mapped IPv6 form must pass the same restrictions as its IPv4 equivalent.
    input
        .parse::<IpAddr>()
        .ok()
        .or_else(|| parse_ipv6_with_decimal_tail(input))
        .map(|address| address.to_canonical())
}

fn parse_ipv6_with_decimal_tail(input: &str) -> Option<IpAddr> {
    let (prefix, tail) = input.rsplit_once(':')?;
    // Java accepts leading zeroes in the embedded IPv4 tail, within its 15-byte bound.
    if !prefix.contains(':') || tail.len() > 15 {
        return None;
    }
    let mut parts = tail.split('.');
    let mut octets = [0; 4];
    for octet in &mut octets {
        let part = parts.next()?;
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    format!("{prefix}:{}", Ipv4Addr::from(octets)).parse().ok()
}

fn resolver_ipv4_alias(name: &str) -> bool {
    let mut parts = name.strip_suffix('.').unwrap_or(name).split('.').peekable();
    for index in 0..4 {
        let Some(value) = parts.next().and_then(numeric_ipv4_part) else {
            return false;
        };
        if parts.peek().is_none() {
            // A legacy address can supply its last 8, 16, 24, or 32 bits as one number.
            return value <= u32::MAX >> (index * 8);
        }
        if value > u32::from(u8::MAX) {
            return false;
        }
    }
    false
}

fn numeric_ipv4_part(part: &str) -> Option<u32> {
    if let Some(hex) = part.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if part.starts_with('0')
        && let Ok(octal) = u32::from_str_radix(part, 8)
    {
        return Some(octal);
    }
    part.parse().ok()
}

fn strip_brackets(input: &str) -> &str {
    input
        .strip_prefix('[')
        .and_then(|input| input.strip_suffix(']'))
        .unwrap_or(input)
}

fn parse_port(input: &str) -> Result<u16, &'static str> {
    parse_java_integer(input)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|&port| port != 0)
        .ok_or("port must be an integer from 1 to 65535")
}

fn parse_java_integer(input: &str) -> Option<i32> {
    if input.is_ascii() {
        return input.parse().ok();
    }
    let mut ascii = String::with_capacity(input.len());
    for digit in input.chars() {
        if digit.is_ascii() {
            ascii.push(digit);
            continue;
        }
        let is_decimal = |character: char| {
            character.len_utf16() == 1 && DECIMAL_DIGIT.is_match(character.encode_utf8(&mut [0; 4]))
        };
        if !is_decimal(digit) {
            return None;
        }
        let code = u32::from(digit);
        let mut start = code;
        while char::from_u32(start - 1).is_some_and(is_decimal) {
            start -= 1;
        }
        ascii.push(char::from_digit((code - start) % 10, 10)?);
    }
    ascii.parse().ok()
}

fn cidr_contains(network: IpAddr, prefix: u8, address: IpAddr) -> bool {
    match (network, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            prefix == 0
                || u32::from(network) >> (32 - prefix) == u32::from(address) >> (32 - prefix)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            prefix == 0
                || u128::from(network) >> (128 - prefix) == u128::from(address) >> (128 - prefix)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, host: &str, port: u16) -> bool {
        let pattern = Pattern::parse(pattern).unwrap();
        Host::parse(host).is_some_and(|host| pattern.matches(&host, port))
    }

    #[test]
    fn matches_names_literals_networks_and_inclusive_ranges() {
        for (pattern, host, port, expected) in [
            (" DB01.example ", "Db01.Example", 2222, true),
            ("db01.example", "db01.example.", 22, false),
            ("*.prod.example", "a.b.prod.example", 22, true),
            ("*.prod.example", "prod.example", 22, false),
            ("web-??.example", "web-01.example", 22, true),
            ("web-??.example", "web-1.example", 22, false),
            ("vault.example:2222", "vault.example", 22, false),
            ("vault.example:٢٢", "vault.example", 22, true),
            ("10.*", "10.2.3.4", 22, false),
            ("192.168.10.42:22", "192.168.10.42", 22, true),
            ("192.168.10.42:22", "192.168.10.42", 2222, false),
            ("[2001:db8::1]:22", "[2001:db8:0:0:0:0:0:1]", 22, true),
            ("[2001:db8::1]:22", "2001:db8::1", 2222, false),
            ("2001:db8::1:22", "2001:db8::1:22", 4711, true),
            ("10.0.0.0/9", "10.127.0.1", 22, true),
            ("10.0.0.0/9", "10.128.0.1", 22, false),
            ("10.0.0.0/8", "::ffff:10.1.2.3", 22, true),
            ("192.0.2.1", "::ffff:192.000.002.001", 22, true),
            ("::ffff:192.000.002.001", "192.0.2.1", 22, true),
            (
                "[2001:db8::192.000.002.001]:22",
                "2001:db8::c000:201",
                22,
                true,
            ),
            ("10.0.0.0/8", "ten.example", 22, false),
            ("0.0.0.0/0", "::1", 22, false),
            ("2001:db8::/32", "2001:db8:1234::1", 2222, true),
            ("2001:db8::/٣٢", "2001:db8:1234::1", 2222, true),
            ("2001:db8::/32", "2001:db9::1", 22, false),
            ("10.20.0.100-10.20.0.199", "10.20.0.100", 22, true),
            ("10.20.0.100-10.20.0.199", "10.20.0.199", 22, true),
            ("10.20.0.100-10.20.0.199", "10.20.0.200", 22, false),
            ("2001:db8::1-2001:db8::2", "2001:db8::2", 2222, true),
            ("*", "\u{3000}", 22, false),
        ] {
            assert_eq!(
                matches(pattern, host, port),
                expected,
                "{pattern} vs {host}:{port}"
            );
        }
    }

    #[test]
    fn malformed_patterns_fail_policy_parsing() {
        for pattern in [
            "",
            "host:0",
            "host:65536",
            "host:abc",
            "host:22:extra",
            "10.0.0.0/33",
            "2001:db8::/129",
            "10.0.0.0/-1",
            "notanip/8",
            "10.0.0.9-10.0.0.1",
            "10.0.0.1-2001:db8::1",
            "[2001:db8::1",
            "[::ffff:10.0.0.1]:22",
            "bad host",
        ] {
            assert!(Pattern::parse(pattern).is_err(), "{pattern}");
        }
        for table in [
            "mode = 'deny'\nhosts = []",
            "mode = 'deny'\nhosts = ['valid', 1]",
            "mode = 'other'\nhosts = ['valid']",
            "mode = 'allow'\nhosts = ['valid', 'bad host']",
            "mode = 'allow'\nhosts = ['valid']\nextra = true",
        ] {
            let source = format!("[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\n{table}");
            assert!(crate::clipboard_policy::parse(&source).is_err(), "{table}");
        }
    }

    #[test]
    fn policy_intersects_restrictions_and_checks_the_route_in_order() {
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n\
             [[rule]]\n[rule.servers]\nmode = ' ALLOW '\nhosts = ['*.example']\n\
             [[rule]]\n[rule.servers]\nmode = 'allow'\nhosts = ['*.example:22']\n\
             [[rule]]\n[rule.servers]\nmode = 'DeNy'\nhosts = ['vault.example', 'bastion.example']",
        )
        .unwrap();
        let servers = policy.server_access().clone();
        assert!(servers.allows("web.example", 22));
        assert!(!servers.allows("web.example", 2222));
        assert!(!servers.allows("other.test", 22));
        assert_eq!(
            servers.first_blocked([("bastion.example", 22), ("vault.example", 22)]),
            Some(("bastion.example", 22))
        );
        assert_eq!(
            servers.first_blocked([("web.example", 22), ("vault.example", 22)]),
            Some(("vault.example", 22))
        );
        assert_eq!(servers.first_blocked([("web.example", 22)]), None);
        assert!(!ServerAccess::lockdown().allows("web.example", 22));
    }

    #[test]
    fn restricted_policy_refuses_numeric_resolver_aliases() {
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\n\
             mode = 'deny'\nhosts = ['127.0.0.0/8']",
        )
        .unwrap();
        let servers = policy.server_access();
        for alias in ["127.1", "0177.0.0.1", "0x7F000001", "0x7f.1", "2130706433"] {
            assert!(matches("*", alias, 22), "Java treats {alias} as a name");
            assert!(!servers.allows(alias, 22), "{alias}");
            assert!(ServerAccess::default().allows(alias, 22), "{alias}");
        }
        assert!(!servers.allows("127.0.0.1", 22));
        assert!(!servers.allows("::ffff:127.0.0.1", 22));
        assert!(!servers.allows("::ffff:127.000.000.001", 22));
        assert!(servers.allows("db.0", 22));
        assert!(servers.allows("192.0.2.1", 22));
    }
}
