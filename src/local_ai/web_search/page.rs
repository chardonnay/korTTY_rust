//! Explicit single-page extraction through Tavily's fixed HTTPS endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

use super::{Connection, Error, Hit, Pending, start_tavily, tavily_json};

pub(crate) const EXTRACT_DESTINATION: &str = "https://api.tavily.com/extract";
pub(crate) const MAX_PAGE_URL_BYTES: usize = 2048;
const TIMEOUT: Duration = Duration::from_secs(40);
const INVALID_URL: &str = "Enter an HTTP or HTTPS URL with a public DNS hostname and default port.";

#[derive(Clone)]
pub(crate) struct PageUrl(Url);

impl PageUrl {
    pub(crate) fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.len() > MAX_PAGE_URL_BYTES
            || raw
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
        {
            return Err(INVALID_URL);
        }
        let (scheme, authority) = raw.split_once("://").ok_or(INVALID_URL)?;
        let authority = authority.split(['/', '?', '#']).next().ok_or(INVALID_URL)?;
        if !(scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http"))
            || authority.is_empty()
            || authority.contains('@')
        {
            return Err(INVALID_URL);
        }
        let mut url = Url::parse(raw).map_err(|_| INVALID_URL)?;
        if !matches!(url.host(), Some(Host::Domain(host)) if web_hostname(host))
            || url.port().is_some()
        {
            return Err(INVALID_URL);
        }
        url.set_query(None);
        url.set_fragment(None);
        let page = Self(url);
        if page.as_str().len() > MAX_PAGE_URL_BYTES
            || !page.decoded().is_ok_and(|text| {
                text.chars().all(|ch| !ch.is_control()) && !text.contains(['%', '\\'])
            })
        {
            return Err(INVALID_URL);
        }
        Ok(page)
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn decoded(&self) -> Result<std::borrow::Cow<'_, str>, &'static str> {
        percent_encoding::percent_decode_str(self.as_str())
            .decode_utf8()
            .map_err(|_| INVALID_URL)
    }
}

fn web_hostname(host: &str) -> bool {
    let Some((_, tld)) = host.rsplit_once('.') else {
        return false;
    };
    !matches!(
        tld,
        "" | "localhost"
            | "local"
            | "internal"
            | "lan"
            | "home"
            | "corp"
            | "onion"
            | "invalid"
            | "test"
            | "example"
            | "arpa"
    ) && tld.bytes().any(|ch| ch.is_ascii_alphabetic())
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
        })
}

#[derive(Serialize)]
struct Request<'a> {
    urls: &'a str,
    extract_depth: &'static str,
    format: &'static str,
    include_images: bool,
}

#[derive(Deserialize)]
struct Response {
    results: Vec<Extraction>,
}

#[derive(Deserialize)]
struct Extraction {
    url: String,
    raw_content: String,
}

pub(crate) fn start_extract<T: Send + 'static>(
    url: PageUrl,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    finish: impl FnOnce(Vec<Hit>) -> T + Send + 'static,
) -> Result<Pending<T>, String> {
    start_tavily(
        TIMEOUT,
        before_dispatch,
        move |connection, key| extract(connection, &url, key),
        finish,
    )
}

fn extract(connection: &Connection, url: &PageUrl, key: &str) -> Result<Vec<Hit>, Error> {
    if !connection.active() {
        return Err(Error::Cancelled);
    }
    if key.is_empty()
        || !url.decoded().is_ok_and(|decoded| !decoded.contains(key))
        || url.as_str().contains(key)
    {
        return Err(Error::Invalid("invalid page address or credential"));
    }
    let response: Response = tavily_json(
        connection,
        "/extract",
        key,
        &Request {
            urls: url.as_str(),
            extract_depth: "basic",
            format: "markdown",
            include_images: false,
        },
    )?;
    Ok(response
        .results
        .into_iter()
        .find_map(|result| {
            let returned = PageUrl::parse(&result.url).ok()?;
            if returned.as_str() != url.as_str()
                || result.raw_content.trim().is_empty()
                || result.raw_content.contains(key)
            {
                return None;
            }
            Some(Hit {
                title: "Extracted webpage".into(),
                url: returned.0.into(),
                content: result.raw_content,
            })
        })
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;
    use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};

    #[test]
    fn tavily_page_url_canonicalizes_before_review_and_rejects_local_addresses() {
        let url = PageUrl::parse("HTTPS://ExAmPle.com:443/docs?q=secret#part").unwrap();
        assert_eq!(url.as_str(), "https://example.com/docs");
        for invalid in [
            "file:///etc/passwd",
            "https:example.com",
            "https:///example.com",
            "https://@example.com/",
            "https://user:secret@example.com/",
            "https://example.com:8080/",
            "https://localhost/",
            "http://hidden.local/",
            "https://2130706433/",
            "https://[::ffff:127.0.0.1]/",
            "https://192.168.0.1/",
            "https://example.com\\secret",
        ] {
            assert!(PageUrl::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn tavily_extract_sends_only_the_reviewed_page_and_validates_returned_source() {
        let (listener, _) = listener();
        let connection = Connection::new(listener.local_addr().unwrap(), TIMEOUT);
        let page = PageUrl::parse("https://example.com/docs").unwrap();
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let body: serde_json::Value =
                serde_json::from_slice(&wire_request(&mut stream, "POST /extract HTTP/1.1"))
                    .unwrap();
            let response = json!({"results":[
                {"url":"https://elsewhere.com/docs", "raw_content":"unreviewed source"},
                {"url":"https://example.com/docs", "raw_content":"embedded test-key"},
                {"url":"https://example.com/docs", "raw_content":"Public evidence"}
            ], "failed_results": []});
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", &response.to_string()).as_bytes())
                .unwrap();
            body
        });
        let result = extract(&connection, &page, "test-key").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "Public evidence");
        assert_eq!(result[0].url, page.as_str());
        assert_eq!(
            server.join().unwrap(),
            json!({"urls":"https://example.com/docs", "extract_depth":"basic", "format":"markdown", "include_images":false})
        );
    }

    #[test]
    fn tavily_extract_rejects_an_encoded_credential_before_dialing() {
        let (listener, _) = listener();
        let connection = Connection::new(listener.local_addr().unwrap(), TIMEOUT);
        let page = PageUrl::parse("https://example.com/%74est-key").unwrap();
        assert!(matches!(
            extract(&connection, &page, "test-key"),
            Err(Error::Invalid(_))
        ));
        drop(connection.cancel_handle());
        assert!(matches!(
            extract(&connection, &page, "test-key"),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
