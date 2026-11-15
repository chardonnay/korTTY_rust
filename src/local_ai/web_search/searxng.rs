//! Explicit page reads through an independently configured SearXNG MCP server.

use serde_json::{Value, json};
use url::Url;

use super::super::validate_credential;
use super::direct_mcp::{Authentication, INVALID, ReadPage, text_result};
use super::{Error, Hit, MAX_PAGE_URL_BYTES, PageUrl, Pending};

pub(super) const TOOL: &str = "web_url_read";
pub(super) const MAX_LENGTH: usize = 4000;
const INVALID_ENDPOINT: &str = "Enter a full SearXNG MCP HTTPS URL or numeric loopback HTTP URL without credentials, a query, or a fragment.";

pub(crate) struct Endpoint(Url);

impl Endpoint {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let invalid = || INVALID_ENDPOINT.to_owned();
        if raw.len() > MAX_PAGE_URL_BYTES
            || raw
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
        {
            return Err(invalid());
        }
        let (scheme, rest) = raw.split_once("://").ok_or_else(invalid)?;
        let (authority, _) = rest.split_once('/').ok_or_else(invalid)?;
        if authority.is_empty() || authority.contains(['@', '?', '#', '%']) {
            return Err(invalid());
        }
        if scheme != "https" && !(scheme == "http" && loopback_authority(authority)) {
            return Err(invalid());
        }
        let endpoint = Url::parse(raw).map_err(|_| invalid())?;
        if endpoint.host().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.port() == Some(0)
            || !endpoint.path().bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'~')
            })
            || endpoint.path().trim_matches('/').is_empty()
            || endpoint.as_str().len() > MAX_PAGE_URL_BYTES
        {
            return Err(invalid());
        }
        Ok(Self(endpoint))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

fn loopback_authority(authority: &str) -> bool {
    ["127.0.0.1", "[::1]"].into_iter().any(|address| {
        authority == address
            || authority
                .strip_prefix(address)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .is_some_and(|port| {
                    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                })
    })
}

pub(super) fn start<T: Send + 'static>(
    endpoint: &str,
    bearer_env: Option<&str>,
    url: PageUrl,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    finish: impl FnOnce(Vec<Hit>) -> T + Send + 'static,
) -> Result<Pending<T>, String> {
    validate_credential(bearer_env)?;
    let endpoint = Endpoint::parse(endpoint)?;
    let arguments = page_arguments(&url);
    ReadPage {
        label: "SearXNG",
        endpoint: endpoint.as_str().to_owned(),
        auth: Authentication::Bearer(bearer_env.map(str::to_owned)),
        tool: TOOL,
    }
    .start(url, arguments, before_dispatch, page_result, finish)
}

fn page_arguments(url: &PageUrl) -> Value {
    json!({"url":url.as_str(),"maxLength":MAX_LENGTH})
}

fn page_result(
    result: Value,
    url: &PageUrl,
    has_secret: &dyn Fn(&str) -> bool,
) -> Result<Vec<Hit>, Error> {
    let text = text_result(&result)?;
    if text.trim().is_empty() || text.chars().count() > MAX_LENGTH || has_secret(text) {
        return Err(INVALID);
    }
    Ok(vec![Hit {
        title: "Requested webpage".into(),
        url: url.as_str().into(),
        content: text.to_owned(),
    }])
}

#[cfg(test)]
mod tests;
