//! Single-page reads through Bright Data's hosted MCP endpoint.

use serde::Deserialize;
use serde_json::{Value, json};

use super::direct_mcp::{Authentication, INVALID, ReadPage, text_result};
use super::{Error, Hit, PageUrl, Pending};

pub(super) const DESTINATION: &str = "https://mcp.brightdata.com/mcp";
pub(super) const TOOL: &str = "scrape_as_markdown";
pub(super) const KEY: &str = "BRIGHT_DATA_API_TOKEN";

pub(super) fn start<T: Send + 'static>(
    url: PageUrl,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    finish: impl FnOnce(Vec<Hit>) -> T + Send + 'static,
) -> Result<Pending<T>, String> {
    let arguments = page_arguments(&url);
    ReadPage {
        label: "Bright Data",
        endpoint: DESTINATION.into(),
        auth: Authentication::Query(KEY),
        tool: TOOL,
    }
    .start(url, arguments, before_dispatch, page_result, finish)
}

fn page_arguments(url: &PageUrl) -> Value {
    json!({"url":url.as_str(),"format":"markdown"})
}

#[derive(Deserialize)]
struct Page {
    url: String,
    #[serde(default)]
    title: Option<String>,
    format: String,
    content: String,
}

fn unwrap_external_page(text: &str) -> Option<&str> {
    let Some(notice) = text.strip_prefix("SECURITY NOTICE: ") else {
        return Some(text);
    };
    let (notice, wrapped) = notice.split_once('\n')?;
    let (begin, wrapped) = wrapped.split_once('\n')?;
    let nonce = begin
        .strip_prefix("=====UNTRUSTED_")?
        .strip_suffix("_BEGIN=====")?;
    if nonce.len() != 32
        || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !notice.contains(&format!("(id {nonce})"))
    {
        return None;
    }
    let (body, end) = wrapped.rsplit_once('\n')?;
    (end == format!("=====UNTRUSTED_{nonce}_END=====")).then_some(body)
}

fn page_result(
    result: Value,
    url: &PageUrl,
    has_secret: &dyn Fn(&str) -> bool,
) -> Result<Vec<Hit>, Error> {
    let page: Page = if let Some(page) = result.get("structuredContent") {
        serde_json::from_value(page.clone()).map_err(|_| INVALID)?
    } else {
        let text = text_result(&result)?;
        serde_json::from_str(unwrap_external_page(text).ok_or(INVALID)?).map_err(|_| INVALID)?
    };
    let title = page
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Extracted webpage".into());
    let returned = PageUrl::parse(&page.url).map_err(|_| INVALID)?;
    if page.url.contains(['?', '#'])
        || returned.as_str() != url.as_str()
        || !matches!(page.format.as_str(), "markdown" | "text")
        || page.content.trim().is_empty()
        || [&page.url, &title, &page.content]
            .iter()
            .any(|text| has_secret(text))
    {
        return Err(INVALID);
    }
    Ok(vec![Hit {
        title,
        url: returned.as_str().into(),
        content: page.content,
    }])
}

#[cfg(test)]
mod tests;
