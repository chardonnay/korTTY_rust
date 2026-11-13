//! Bounded transport for installation-policy model downloads.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ureq::http::{HeaderMap, header};
use ureq::{Agent, Body};
use url::Url;

use super::super::connection_config;
use super::super::managed_models::MAX_MODEL_BYTES;

const MAX_REDIRECTS: usize = 5;
const MAX_URL_BYTES: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const POLICY_INTERVAL: Duration = Duration::from_millis(250);

pub(super) fn validate_url(url: &str, allow_http: bool) -> io::Result<()> {
    parse_url(url, allow_http).map(|_| ())
}

pub(super) fn receive(
    url: &str,
    allow_http: bool,
    destination: &mut impl Write,
    cancelled: &AtomicBool,
    deadline: Instant,
    require_current: &dyn Fn() -> io::Result<()>,
) -> io::Result<u64> {
    receive_bounded(
        url,
        allow_http,
        destination,
        Guard {
            cancelled,
            deadline,
            require_current,
        },
        MAX_MODEL_BYTES,
    )
}

struct Guard<'a> {
    cancelled: &'a AtomicBool,
    deadline: Instant,
    require_current: &'a dyn Fn() -> io::Result<()>,
}

impl Guard<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Policy model download cancelled",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(transfer_timeout());
        }
        Ok(remaining)
    }

    fn check_policy(&self) -> io::Result<Duration> {
        self.remaining()?;
        (self.require_current)()?;
        self.remaining()
    }
}

fn receive_bounded(
    url: &str,
    allow_http: bool,
    destination: &mut impl Write,
    guard: Guard<'_>,
    max_bytes: u64,
) -> io::Result<u64> {
    let mut url = parse_url(url, allow_http)?;
    let agent = Agent::new_with_config(connection_config(guard.remaining()?));
    for hops in 0..=MAX_REDIRECTS {
        let remaining = guard.check_policy()?;
        let response = agent
            .get(url.as_str())
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::ACCEPT, "application/octet-stream")
            .header(header::CONNECTION, "close")
            .header(header::USER_AGENT, "kortty-policy-model")
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .timeout_resolve(Some(READ_TIMEOUT))
            .timeout_per_read(Some(READ_TIMEOUT))
            .timeout_recv_response(Some(READ_TIMEOUT))
            .build()
            .call();
        guard.remaining()?;
        let mut response = response.map_err(|error| {
            if matches!(error, ureq::Error::Timeout(_)) {
                transfer_timeout()
            } else {
                // A configured URL or a delegated location may contain a signed query.
                io::Error::other("Could not connect to the policy model source")
            }
        })?;
        match response.status().as_u16() {
            200 => {
                let length = response_length(response.headers(), max_bytes)?;
                return copy_body(response.body_mut(), destination, &guard, length, max_bytes);
            }
            301 | 302 | 303 | 307 | 308 if hops < MAX_REDIRECTS => {
                let location = single_header(response.headers(), header::LOCATION)?
                    .ok_or_else(invalid_redirect)?;
                url = redirect(&url, location, allow_http)?;
            }
            301 | 302 | 303 | 307 | 308 => {
                return Err(invalid("The policy model source redirected too many times"));
            }
            status => {
                return Err(io::Error::other(format!(
                    "The policy model source returned HTTP {status}"
                )));
            }
        }
    }
    unreachable!("the final response cannot redirect again");
}

fn parse_url(value: &str, allow_http: bool) -> io::Result<Url> {
    let scheme = value.split_once("://").map(|(scheme, _)| scheme);
    if !valid_url_text(value)
        || !scheme.is_some_and(|scheme| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        })
    {
        return Err(invalid("The policy model source URL is invalid"));
    }
    let url = Url::parse(value).map_err(|_| invalid("The policy model source URL is invalid"))?;
    require_destination(&url, allow_http)?;
    Ok(url)
}

fn require_destination(url: &Url, allow_http: bool) -> io::Result<()> {
    if !matches!(url.scheme(), "https" | "http")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.as_str().len() > MAX_URL_BYTES
    {
        return Err(invalid("The policy model source URL is invalid"));
    }
    if url.scheme() == "http" && !allow_http {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "HTTP policy model sources require a configured SHA-256 digest",
        ));
    }
    Ok(())
}

fn redirect(current: &Url, location: &str, allow_http: bool) -> io::Result<Url> {
    if !valid_url_text(location) {
        return Err(invalid_redirect());
    }
    let next = current.join(location).map_err(|_| invalid_redirect())?;
    require_destination(&next, allow_http).map_err(|_| invalid_redirect())?;
    let allowed = if current.scheme() == "https" {
        next.scheme() == "https"
    } else {
        current.origin() == next.origin()
            || (next.scheme() == "https" && current.host() == next.host() && next.port().is_none())
    };
    if !allowed {
        return Err(invalid_redirect());
    }
    Ok(next)
}

fn valid_url_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_URL_BYTES
        && !value.contains('\\')
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
}

fn response_length(headers: &HeaderMap, max_bytes: u64) -> io::Result<Option<u64>> {
    if headers.contains_key(header::CONTENT_RANGE)
        || single_header(headers, header::CONTENT_ENCODING)?
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(invalid(
            "Expected a complete, unencoded policy model response",
        ));
    }
    let length = single_header(headers, header::CONTENT_LENGTH)?;
    if let Some(coding) = single_header(headers, header::TRANSFER_ENCODING)?
        && (length.is_some() || !coding.eq_ignore_ascii_case("chunked"))
    {
        return Err(invalid("Invalid policy model transfer framing"));
    }
    length
        .map(|value| {
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_size());
            }
            value
                .parse::<u64>()
                .ok()
                .filter(|size| (1..=max_bytes).contains(size))
                .ok_or_else(invalid_size)
        })
        .transpose()
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> io::Result<Option<&str>> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid(
            "The policy model source returned ambiguous headers",
        ));
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| invalid("The policy model source returned an invalid header"))
}

fn copy_body(
    body: &mut Body,
    destination: &mut impl Write,
    guard: &Guard<'_>,
    length: Option<u64>,
    max_bytes: u64,
) -> io::Result<u64> {
    guard.check_policy()?;
    let mut next_check = Instant::now() + POLICY_INTERVAL;
    let mut reader = body.as_reader();
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0;
    loop {
        guard.remaining()?;
        let read = reader.read(&mut buffer);
        guard.remaining()?;
        let count = read.map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut {
                transfer_timeout()
            } else {
                invalid("Could not receive the complete policy model response")
            }
        })?;
        if count == 0 {
            break;
        }
        if Instant::now() >= next_check {
            guard.check_policy()?;
            next_check = Instant::now() + POLICY_INTERVAL;
        }
        received += count as u64;
        if received > max_bytes || length.is_some_and(|length| received > length) {
            return Err(invalid_size());
        }
        destination.write_all(&buffer[..count])?;
    }
    if received == 0 || length.is_some_and(|length| received != length) {
        return Err(invalid_size());
    }
    guard.check_policy()?;
    Ok(received)
}

fn transfer_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "The policy model download timed out",
    )
}

fn invalid_redirect() -> io::Error {
    invalid("The policy model source returned an unsafe redirect")
}

fn invalid_size() -> io::Error {
    invalid("The policy model response has an invalid or excessive byte count")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream;
    use std::thread;

    use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};

    use super::*;

    fn expect_download(stream: &mut BufReader<TcpStream>, target: &str) {
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("GET {target} HTTP/1.1"));
        loop {
            line.clear();
            assert!(stream.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            let (name, _) = line.split_once(':').unwrap();
            assert!(
                !["authorization", "proxy-authorization", "cookie"]
                    .iter()
                    .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
            );
        }
    }

    fn download_fixture(
        url: &str,
        destination: &mut impl Write,
        max_bytes: u64,
        require_current: &dyn Fn() -> io::Result<()>,
    ) -> io::Result<u64> {
        receive_bounded(
            url,
            true,
            destination,
            Guard {
                cancelled: &AtomicBool::new(false),
                deadline: Instant::now() + Duration::from_secs(4),
                require_current,
            },
            max_bytes,
        )
    }

    #[test]
    fn policy_model_http_streams_exact_bytes_and_redirects_without_cookies() {
        let (server, _) = listener();
        let url = format!("http://{}/model", server.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let mut first = accept(&server);
            expect_download(&mut first, "/model");
            first
                .get_mut()
                .write_all(
                    fixture_http_response(
                        "302 Found",
                        "Location: /asset?signature=fixture\r\nSet-Cookie: rejected=fixture\r\n",
                        "",
                    )
                    .as_bytes(),
                )
                .unwrap();
            let mut second = accept(&server);
            expect_download(&mut second, "/asset?signature=fixture");
            second
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", "GGUFdata").as_bytes())
                .unwrap();
        });
        let mut bytes = Vec::new();
        assert_eq!(
            download_fixture(&url, &mut bytes, 8, &|| Ok(())).unwrap(),
            8
        );
        assert_eq!(bytes, b"GGUFdata");
        worker.join().unwrap();
    }

    #[test]
    fn policy_model_http_rechecks_authority_before_redirect_dispatch() {
        let (server, _) = listener();
        let url = format!("http://{}/model", server.local_addr().unwrap());
        let allowed = AtomicBool::new(true);
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let mut first = accept(&server);
                wire_request(&mut first, "GET /model HTTP/1.1");
                allowed.store(false, Ordering::Release);
                first
                    .get_mut()
                    .write_all(
                        fixture_http_response("302 Found", "Location: /asset\r\n", "").as_bytes(),
                    )
                    .unwrap();
            });
            let require_current = || {
                if allowed.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(io::ErrorKind::PermissionDenied.into())
                }
            };
            let mut bytes = Vec::new();
            assert_eq!(
                download_fixture(&url, &mut bytes, 8, &require_current)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            assert!(bytes.is_empty());
            worker.join().unwrap();
        });
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn policy_model_http_rejects_oversized_and_partial_responses() {
        for response in [
            fixture_http_response("200 OK", "", "123456789"),
            fixture_http_response("206 Partial Content", "Content-Range: bytes 0-2/8\r\n", "abc"),
            fixture_http_response("200 OK", "Content-Encoding: gzip\r\n", "12345"),
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n9\r\n123456789\r\n0\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nshort".into(),
        ] {
            let (server, _) = listener();
            let url = format!("http://{}/model", server.local_addr().unwrap());
            let worker = thread::spawn(move || {
                let mut stream = accept(&server);
                wire_request(&mut stream, "GET /model HTTP/1.1");
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            });
            let mut bytes = Vec::new();
            assert!(download_fixture(&url, &mut bytes, 8, &|| Ok(())).is_err());
            assert!(bytes.len() <= 8);
            worker.join().unwrap();
        }
    }

    #[test]
    fn policy_model_http_rejects_unpinned_http_and_unsafe_locations() {
        let https = parse_url("https://models.example.invalid/gguf", false).unwrap();
        let http = parse_url("http://models.example.invalid/gguf", true).unwrap();
        assert_eq!(
            validate_url(http.as_str(), false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(redirect(&https, http.as_str(), true).is_err());
        assert!(redirect(&http, "//different.example.invalid/asset", true).is_err());
        assert!(redirect(&http, https.as_str(), true).is_ok());
        assert!(
            redirect(
                &https,
                "https://cdn.example.invalid/asset?signature=test",
                false
            )
            .is_ok()
        );
        for location in [
            "https://user:password@cdn.example.invalid/asset",
            "https://cdn.example.invalid/asset#fragment",
            "https://cdn.example.invalid\\asset",
            "https://cdn.example.invalid/asset\r\n",
        ] {
            assert!(redirect(&https, location, false).is_err());
            assert!(validate_url(location, false).is_err());
        }
    }

    #[test]
    fn policy_model_http_checks_cancellation_and_expired_deadline_before_connecting() {
        let (server, _) = listener();
        let url = format!("http://{}/model", server.local_addr().unwrap());
        for (cancelled, deadline, expected) in [
            (
                true,
                Instant::now() + Duration::from_secs(4),
                io::ErrorKind::Interrupted,
            ),
            (false, Instant::now(), io::ErrorKind::TimedOut),
        ] {
            let mut bytes = Vec::new();
            let result = receive(
                &url,
                true,
                &mut bytes,
                &AtomicBool::new(cancelled),
                deadline,
                &|| panic!("stopped downloads must never authorize a request"),
            );
            assert_eq!(result.unwrap_err().kind(), expected);
            assert!(bytes.is_empty());
            assert_eq!(
                server.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }
}
