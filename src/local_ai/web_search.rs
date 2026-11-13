//! Explicit, bounded web requests with their own approval.

use super::{
    Activity, Agent, Cancel, Connection, Error, Pending, WorkerPermit, bearer_header,
    connection_config, credential_value, response_bytes, start_worker,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

mod mcp;
pub(crate) use mcp::{Mode as McpMode, Settings as McpSettings, ToolOutput};

const BASE: &str = "https://api.tavily.com";
pub(crate) const DESTINATION: &str = "https://api.tavily.com/search";
const KEY: &str = "TAVILY_API_KEY";
pub(crate) const MAX_QUERY_BYTES: usize = 400;
pub(crate) const MAX_RESULTS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Deserialize)]
struct Response {
    results: Vec<Hit>,
}

#[derive(Deserialize)]
pub(crate) struct Hit {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) content: String,
}

#[derive(Serialize)]
struct Request<'a> {
    query: &'a str,
    search_depth: &'static str,
    max_results: usize,
    include_answer: bool,
    include_raw_content: bool,
    include_images: bool,
}

pub(crate) fn start<T: Send + 'static>(
    query: String,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    finish: impl FnOnce(Vec<Hit>) -> T + Send + 'static,
) -> Result<Pending<T>, String> {
    let permit = WorkerPermit::acquire().ok_or("AI workers are busy.")?;
    let activity = Arc::new(Activity::default());
    let cancel = Cancel {
        activity: Arc::clone(&activity),
    };
    start_worker(cancel, move || {
        let _permit = permit;
        require_policy()?;
        let key = credential_value(KEY).map_err(|_| "Set TAVILY_API_KEY to use web search.")?;
        let agent = Agent::new_with_config(connection_config(TIMEOUT));
        let connection = Connection::attach(BASE.into(), agent, activity)
            .map_err(|_| "Web search was canceled.")?;
        before_dispatch()?;
        let result = search(&connection, &query, &key).map_err(message)?;
        before_dispatch()?;
        Ok(finish(result))
    })
}

pub(crate) fn start_mcp<T: Send + 'static>(
    profile: super::Profile,
    settings: McpSettings,
    query: String,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
    finish: impl FnOnce(Vec<ToolOutput>) -> T + Send + 'static,
) -> Result<Pending<T>, String> {
    super::start(profile, move |client| {
        let result = mcp::search(client, &settings, &query, before_dispatch)?;
        Ok(finish(result))
    })
}

pub(crate) fn require_policy() -> Result<(), String> {
    let policy = crate::clipboard_policy::ClipboardPolicy::load();
    if policy.ai_allowed && policy.ai_chat_allowed && policy.ai_web_search_allowed {
        Ok(())
    } else {
        Err("Managed policy blocks web search.".into())
    }
}

fn search(connection: &Connection, query: &str, key: &str) -> Result<Vec<Hit>, Error> {
    if !connection.active() {
        return Err(Error::Cancelled);
    }
    if query.trim().is_empty()
        || query.len() > MAX_QUERY_BYTES
        || key.is_empty()
        || query.contains(key)
    {
        return Err(Error::Invalid("invalid search query or credential"));
    }
    let request = Request {
        query,
        search_depth: "basic",
        max_results: MAX_RESULTS,
        include_answer: false,
        include_raw_content: false,
        include_images: false,
    };
    let request = zeroize::Zeroizing::new(
        serde_json::to_vec(&request).map_err(|_| Error::Invalid("invalid search query"))?,
    );
    #[cfg(target_os = "macos")]
    let _power = kortty_macos::AppNapActivity::begin();
    let mut response = connection
        .agent
        .post(connection.url("/search"))
        .header("Authorization", bearer_header(key)?)
        .header("Content-Type", "application/json")
        .send(request.as_slice())
        .map_err(|error| match error {
            ureq::Error::StatusCode(status) => Error::Status(status),
            _ => super::transport_error(error, true),
        })?;
    let bytes = response_bytes(&mut response, true)?;
    if !connection.active() {
        return Err(Error::Cancelled);
    }
    let response: Response =
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("invalid search result"))?;
    Ok(response
        .results
        .into_iter()
        .take(MAX_RESULTS)
        .filter(|hit| {
            [&hit.title, &hit.url, &hit.content]
                .iter()
                .all(|value| !value.contains(key))
        })
        .collect())
}

fn message(error: Error) -> String {
    match error {
        Error::Status(status) => format!("Tavily returned HTTP {status}."),
        Error::TimedOut => "The Tavily request timed out.".into(),
        Error::Cancelled => "Web search was canceled.".into(),
        Error::Invalid(reason) => format!("Tavily search was rejected ({reason})."),
        _ => "Could not complete the Tavily search.".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serde_json::json;

    use super::*;
    use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};

    #[test]
    fn tavily_sends_only_the_reviewed_query_and_drops_echoed_credentials() {
        let (listener, _) = listener();
        let connection = Connection::new(listener.local_addr().unwrap(), TIMEOUT);
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let body: serde_json::Value =
                serde_json::from_slice(&wire_request(&mut stream, "POST /search HTTP/1.1"))
                    .unwrap();
            let response = json!({
                "query": "Ignored server echo",
                "results": [
                    {"title": "Reference", "url": "https://example.com/", "content": "A usable excerpt"},
                    {"title": "Fixture credential", "url": "https://example.com/private", "content": "tvly-fixture-key"}
                ]
            });
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", &response.to_string()).as_bytes())
                .unwrap();
            body
        });
        let results = search(&connection, "Only the masked query ***", "tvly-fixture-key").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Reference");
        assert_eq!(
            server.join().unwrap(),
            json!({
                "query": "Only the masked query ***",
                "search_depth": "basic",
                "max_results": 5,
                "include_answer": false,
                "include_raw_content": false,
                "include_images": false
            })
        );
    }

    #[test]
    fn tavily_refuses_unreviewable_queries_without_dialing() {
        let (listener, _) = listener();
        let connection = Connection::new(listener.local_addr().unwrap(), TIMEOUT);
        assert!(matches!(
            search(&connection, "echo fixture-key", "fixture-key"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            search(&connection, "query", ""),
            Err(Error::Invalid(_))
        ));
        drop(connection.cancel_handle());
        assert!(matches!(
            search(&connection, "query", "fixture-key"),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
