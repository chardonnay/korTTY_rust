use std::io::{BufRead, Read, Write};
use std::thread;
use std::time::Duration;

use crate::local_ai::Connection;
use crate::local_ai::test_support::{accept, fixture_http_response, listener};
use crate::local_ai::web_search::direct_mcp::{ToolCall, secret_checker};

use super::*;

const PAGE_URL: &str = "https://public.example.com/docs";

#[test]
fn validates_the_complete_searxng_mcp_endpoint() {
    for (raw, canonical) in [
        (
            "https://MCP.Example.com:443/mcp",
            "https://mcp.example.com/mcp",
        ),
        (
            "https://mcp.example.com:444/http/mcp",
            "https://mcp.example.com:444/http/mcp",
        ),
        ("http://127.0.0.1:3636/mcp", "http://127.0.0.1:3636/mcp"),
        ("http://[::1]:3636/mcp", "http://[::1]:3636/mcp"),
    ] {
        assert_eq!(Endpoint::parse(raw).unwrap().as_str(), canonical);
    }
    for raw in [
        "http://localhost/mcp",
        "http://example.com/mcp",
        "http://127.1/mcp",
        "http://127.0.0.2/mcp",
        "http://2130706433/mcp",
        "http://[::ffff:127.0.0.1]/mcp",
        "https:///example.com/mcp",
        "https://example.com",
        "https://example.com/",
        "https://example.com:0/mcp",
        "https://u:p@example.com/mcp",
        "https://@example.com/mcp",
        "https://example.com/mcp?token=fixture",
        "https://example.com/mcp#part",
        "https://example.com/mcp%0a",
        "https://example.com\\mcp",
    ] {
        assert!(Endpoint::parse(raw).is_err(), "{raw}");
    }
}

#[test]
fn searxng_sends_only_the_reviewed_url_and_retains_requested_provenance() {
    let (listener, _) = listener();
    let endpoint = Endpoint::parse(&format!(
        "http://{}/proxy/mcp",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let connection = Connection::with_base(endpoint.as_str().into(), Duration::from_secs(5));
    let fixture = thread::spawn(move || {
        for method in ["initialize", "notifications/initialized", "tools/call"] {
            let mut stream = accept(&listener);
            let mut head = String::new();
            stream.read_line(&mut head).unwrap();
            assert_eq!(head.trim_end(), "POST /proxy/mcp HTTP/1.1");
            let mut header = String::new();
            loop {
                let bytes = stream.read_line(&mut header).unwrap();
                assert_ne!(bytes, 0);
                if header.ends_with("\r\n\r\n") {
                    break;
                }
            }
            let headers = header.to_ascii_lowercase();
            assert!(headers.contains("authorization: bearer test+key\r\n"));
            assert_eq!(
                headers.contains("mcp-protocol-version: 2025-11-25\r\n"),
                method != "initialize"
            );
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], method);
            let response = match method {
                "initialize" => {
                    assert_eq!(request["params"]["protocolVersion"], "2025-06-18");
                    json!({"jsonrpc":"2.0","id":1,"result":{
                        "protocolVersion":"2025-11-25","capabilities":{"tools":{}}
                    }})
                }
                "notifications/initialized" => {
                    stream
                        .get_mut()
                        .write_all(fixture_http_response("202 Accepted", "", "").as_bytes())
                        .unwrap();
                    continue;
                }
                "tools/call" => {
                    assert_eq!(
                        request,
                        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                            "name":"web_url_read","arguments":{"url":PAGE_URL,"maxLength":4000}
                        }})
                    );
                    json!({"jsonrpc":"2.0","id":2,"result":{
                        "content":[{"type":"text","text":"An excerpt [with a link](https://other.example.com/)."}]
                    }})
                }
                _ => unreachable!(),
            };
            stream
                .get_mut()
                .write_all(
                    fixture_http_response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        &response.to_string(),
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
    });
    let page = PageUrl::parse(PAGE_URL).unwrap();
    let result = ToolCall {
        auth: &Authentication::Bearer(Some("MCP_SEARXNG_TOKEN".into())),
        key: Some("test+key"),
        url: &page,
        tool: TOOL,
        arguments: page_arguments(&page),
    }
    .fetch(&connection, || Ok(()), page_result)
    .unwrap();
    fixture.join().unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].title, "Requested webpage");
    assert_eq!(result[0].url, PAGE_URL);
    assert_eq!(
        result[0].content,
        "An excerpt [with a link](https://other.example.com/)."
    );
}

#[test]
fn searxng_rejects_empty_multi_block_oversized_and_credential_reflecting_evidence() {
    let page = PageUrl::parse(PAGE_URL).unwrap();
    let text = |text: &str| json!({"content":[{"type":"text","text":text}]});
    for invalid in [
        text(""),
        text(&"é".repeat(MAX_LENGTH + 1)),
        text("The provider echoed test%2Bkey."),
        json!({"content":[{"type":"image","data":"x","mimeType":"image/png"}]}),
        json!({"content":[{"type":"text","text":"one"},{"type":"text","text":"two"}]}),
    ] {
        assert!(page_result(invalid, &page, &secret_checker("test+key")).is_err());
    }
}
