//! Anonymous, bounded downloads from the dedicated catalog release channel.

use std::io::{self, Read as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ureq::Agent;
use ureq::http::{StatusCode, header};
use url::Url;

use super::{
    CATALOG_URL, Catalog, MAX_CATALOG_BYTES, MAX_SIGNATURE_BYTES, Repository, SIGNATURE_URL, Trust,
};

const CHANNEL_HOST: &str = "github.com";
const RELEASE_PATH: &str = "/chardonnay/kortty-ai-catalog/releases/";
const MAX_REDIRECTS: usize = 5;
const MAX_REDIRECT_BYTES: usize = 8192;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Pending {
    pub(crate) reply: async_channel::Receiver<Completion>,
    pub(crate) cancel: Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct Completion {
    pub(crate) catalog: Option<Catalog>,
    pub(crate) warning: Option<String>,
}

pub(crate) struct Cancel(Arc<AtomicBool>, Agent);

impl Cancel {
    pub(crate) fn request(&self) {
        if !self.0.swap(true, Ordering::AcqRel) {
            self.1.clone().shutdown();
        }
    }
}

impl Drop for Cancel {
    fn drop(&mut self) {
        self.request();
    }
}

pub(crate) fn start(
    root: PathBuf,
    trust: Trust,
    authorize: impl Fn() -> io::Result<()> + Send + 'static,
) -> Result<Pending, String> {
    let agent = Agent::new_with_config(super::super::connection_config(REFRESH_TIMEOUT));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = Cancel(Arc::clone(&cancelled), agent.clone());
    let (send, reply) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("kortty-ai-catalog".into())
        .spawn(move || {
            let guard = Guard {
                cancelled: &cancelled,
                deadline: Instant::now() + REFRESH_TIMEOUT,
                authorize: &authorize,
            };
            let repository = Repository::new(&root, trust);
            let (accepted, warning) = match refresh(&repository, &agent, guard) {
                Ok(catalog) => (Some(catalog), None),
                Err(error) => (None, Some(error.to_string())),
            };
            let cached = repository.load_cached().ok().flatten();
            let catalog = [accepted, cached]
                .into_iter()
                .flatten()
                .max_by_key(Catalog::sequence);
            let _ = send.try_send(Completion { catalog, warning });
        })
        .map_err(|error| format!("Could not start the AI catalog refresh ({error})."))?;
    Ok(Pending { reply, cancel })
}

#[derive(Clone, Copy)]
struct Guard<'a> {
    cancelled: &'a AtomicBool,
    deadline: Instant,
    authorize: &'a dyn Fn() -> io::Result<()>,
}

impl Guard<'_> {
    fn remaining(self) -> io::Result<Duration> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "The AI catalog refresh was canceled.",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The AI catalog refresh timed out.",
            ));
        }
        Ok(remaining)
    }

    fn check(self) -> io::Result<Duration> {
        self.remaining()?;
        (self.authorize)()?;
        self.remaining()
    }
}

fn refresh(repository: &Repository<'_>, agent: &Agent, guard: Guard<'_>) -> io::Result<Catalog> {
    let catalog = fetch(agent, CATALOG_URL, MAX_CATALOG_BYTES, guard)?;
    let signature = fetch(agent, SIGNATURE_URL, MAX_SIGNATURE_BYTES, guard)?;
    repository.accept(&catalog, &signature, || guard.check().map(|_| ()))
}

fn fetch(agent: &Agent, endpoint: &str, limit: usize, guard: Guard<'_>) -> io::Result<Vec<u8>> {
    let url = Url::parse(endpoint).map_err(|_| bad_redirect())?;
    let asset = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .ok_or_else(bad_redirect)?
        .to_owned();
    fetch_asset(agent, url, limit, guard, |current, next| {
        allowed_redirect(current, next, &asset)
    })
}

fn fetch_asset(
    agent: &Agent,
    mut url: Url,
    limit: usize,
    guard: Guard<'_>,
    allow_redirect: impl Fn(&Url, &Url) -> bool,
) -> io::Result<Vec<u8>> {
    for hops in 0..=MAX_REDIRECTS {
        let remaining = guard.check()?;
        let response = agent
            .get(url.as_str())
            .header(
                header::ACCEPT,
                "application/json, text/plain, application/octet-stream",
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::USER_AGENT, "korTTY-ai-catalog/1")
            .config()
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .timeout_resolve(Some(IO_TIMEOUT))
            .timeout_recv_response(Some(IO_TIMEOUT))
            .timeout_per_read(Some(IO_TIMEOUT))
            .build()
            .call();
        guard.remaining()?;
        // A CDN redirect can include signed query parameters. Keep raw transport errors private.
        let mut response = response
            .map_err(|_| io::Error::other("Could not contact the AI catalog release channel."))?;
        let status = response.status();
        if status == StatusCode::OK {
            if let Some(encoding) = response.headers().get(header::CONTENT_ENCODING)
                && !encoding.as_bytes().eq_ignore_ascii_case(b"identity")
            {
                return Err(super::invalid(
                    "The AI catalog response has an unexpected content encoding.",
                ));
            }
            let mut bytes = Vec::new();
            let read = response
                .body_mut()
                .as_reader()
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes);
            guard.remaining()?;
            read.map_err(|_| io::Error::other("Could not read the AI catalog release asset."))?;
            if bytes.is_empty() || bytes.len() > limit {
                return Err(super::invalid(
                    "The AI catalog release asset has an invalid size.",
                ));
            }
            return Ok(bytes);
        }
        if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Err(io::Error::other(format!(
                "The AI catalog release channel returned HTTP {}.",
                status.as_u16()
            )));
        }
        if hops == MAX_REDIRECTS {
            return Err(super::invalid(
                "The AI catalog release channel redirected too many times.",
            ));
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= MAX_REDIRECT_BYTES
                    && !value
                        .bytes()
                        .any(|byte| byte.is_ascii_control() || byte == b'\\')
            })
            .ok_or_else(bad_redirect)?;
        let next = url.join(location).map_err(|_| bad_redirect())?;
        if !allow_redirect(&url, &next) {
            return Err(bad_redirect());
        }
        url = next;
    }
    unreachable!("the redirect loop checks its bound")
}

fn allowed_redirect(current: &Url, next: &Url, asset: &str) -> bool {
    if next.scheme() != "https"
        || next.port().is_some()
        || !next.username().is_empty()
        || next.password().is_some()
        || next.fragment().is_some()
    {
        return false;
    }
    if next.host_str() == Some(CHANNEL_HOST) && current.host_str() == Some(CHANNEL_HOST) {
        return next.query().is_none()
            && next.path().strip_prefix(RELEASE_PATH).is_some_and(|path| {
                path.strip_prefix("download/")
                    .and_then(|path| path.rsplit_once('/'))
                    .is_some_and(|(tag, filename)| valid_tag(tag) && filename == asset)
            });
    }
    let next_cdn = matches!(
        next.host_str(),
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
    );
    next_cdn
        && (current.host_str() == Some(CHANNEL_HOST) || current.origin() == next.origin())
        && next
            .path()
            .strip_prefix("/github-production-release-asset/")
            .is_some_and(|path| !path.is_empty())
}

fn valid_tag(tag: &str) -> bool {
    tag.strip_prefix("model-prompt-catalog-")
        .is_some_and(|version| {
            (1..=128).contains(&version.len())
                && version
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn bad_redirect() -> io::Error {
    super::invalid("The AI catalog release channel redirected outside its approved destinations.")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{BufRead as _, Write as _};

    use super::*;
    use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};

    #[test]
    fn ai_catalog_online_pins_release_and_cdn_redirects() {
        let parse = |url| Url::parse(url).unwrap();
        let start = parse(CATALOG_URL);
        let asset = "model-prompt-catalog-v1.json";
        let release = parse(
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/model-prompt-catalog-3.1_rc1/model-prompt-catalog-v1.json",
        );
        let cdn = parse(
            "https://release-assets.githubusercontent.com/github-production-release-asset/123/asset-id?token=fixture",
        );
        assert!(allowed_redirect(&start, &release, asset));
        assert!(allowed_redirect(&release, &cdn, asset));
        assert!(allowed_redirect(&cdn, &cdn, asset));
        assert!(!allowed_redirect(&cdn, &release, asset));
        for url in [
            "https://github.com/chardonnay/kortty-llama-runtimes/releases/download/model-prompt-catalog-3.1/model-prompt-catalog-v1.json",
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/model-prompt-catalog-3.1/model-prompt-catalog-v1.sig",
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/other-tag/model-prompt-catalog-v1.json",
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/model-prompt-catalog-3.1/model-prompt-catalog-v1.json?token=fixture",
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/model-prompt-catalog-3.1/model-prompt-catalog-v1.json#fragment",
            "https://github.com/chardonnay/kortty-ai-catalog/releases/download/model-prompt-catalog-3.1%2Fextra/model-prompt-catalog-v1.json",
            "https://release-assets.githubusercontent.com/unrelated/123?token=fixture",
            "https://release-assets.githubusercontent.com.example.test/github-production-release-asset/123",
            "https://user:password@release-assets.githubusercontent.com/github-production-release-asset/123",
            "https://release-assets.githubusercontent.com:444/github-production-release-asset/123",
            "http://release-assets.githubusercontent.com/github-production-release-asset/123",
        ] {
            assert!(
                !allowed_redirect(&release, &parse(url), asset),
                "approved {url}"
            );
        }
    }

    #[test]
    fn ai_catalog_online_rechecks_authorization_before_anonymous_redirect() {
        let (listener, _) = listener();
        let endpoint =
            Url::parse(&format!("http://{}/first", listener.local_addr().unwrap())).unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let mut headers = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line.to_ascii_lowercase());
            }
            assert!(headers.starts_with("get /first http/1.1\r\n"));
            assert!(!headers.contains("authorization:"));
            assert!(!headers.contains("cookie:"));
            assert!(headers.contains("accept-encoding: identity"));
            stream
                .get_mut()
                .write_all(
                    fixture_http_response("302 Found", "Location: /second\r\n", "").as_bytes(),
                )
                .unwrap();
            listener
        });
        let checked = Cell::new(0);
        let authorize = || {
            checked.set(checked.get() + 1);
            if checked.get() > 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Review expired",
                ));
            }
            Ok(())
        };
        let agent = Agent::new_with_config(super::super::super::connection_config(IO_TIMEOUT));
        let cancelled = AtomicBool::new(false);
        let guard = Guard {
            cancelled: &cancelled,
            deadline: Instant::now() + IO_TIMEOUT,
            authorize: &authorize,
        };
        let result = fetch_asset(&agent, endpoint, 8, guard, |current, next| {
            current.origin() == next.origin()
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(checked.get(), 2);
        assert_eq!(
            worker.join().unwrap().accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn ai_catalog_online_bounds_response_and_honors_cancellation() {
        let (listener, _) = listener();
        let endpoint = Url::parse(&format!(
            "http://{}/fixture",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            wire_request(&mut stream, "GET /fixture HTTP/1.1");
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", "12345").as_bytes())
                .unwrap();
            listener
        });
        let agent = Agent::new_with_config(super::super::super::connection_config(IO_TIMEOUT));
        let cancelled = Arc::new(AtomicBool::new(false));
        let guard = Guard {
            cancelled: &cancelled,
            deadline: Instant::now() + IO_TIMEOUT,
            authorize: &|| Ok(()),
        };
        let result = fetch_asset(&agent, endpoint.clone(), 4, guard, |_, _| false);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let listener = worker.join().unwrap();
        let cancel = Cancel(Arc::clone(&cancelled), agent.clone());
        cancel.request();
        let result = fetch_asset(&agent, endpoint, 8, guard, |_, _| false);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
