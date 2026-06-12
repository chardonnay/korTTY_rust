//! GitHub "latest release" client, ported from Java `GitHubReleaseClient`.

use super::{UpdateAsset, UpdateRelease};
use serde_json::{Map, Value};
use std::time::Duration;

/// Latest-release endpoint for the Rust port. The Java client points at
/// `chardonnay/korTTY`; this port uses its own repository
/// (`git remote origin` = `https://github.com/chardonnay/korTTY_rust`).
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/chardonnay/korTTY_rust/releases/latest";

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const USER_AGENT: &str = "KorTTY-UpdateChecker";

/// Fetches and parses the latest release from GitHub.
pub async fn fetch_latest_release() -> Result<UpdateRelease, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Could not create HTTP client: {e}"))?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("GitHub release request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "GitHub release request failed with HTTP {}",
            status.as_u16()
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|e| format!("GitHub release response could not be read: {e}"))?;
    parse_release(&body)
}

/// Parses a GitHub release JSON document (mirrors Java
/// `GitHubReleaseClient.parseRelease`, including lenient asset handling).
pub fn parse_release(response_body: &str) -> Result<UpdateRelease, String> {
    let parsed: Value = serde_json::from_str(response_body)
        .map_err(|_| "GitHub release response was not a JSON object.".to_string())?;
    let root = parsed
        .as_object()
        .ok_or_else(|| "GitHub release response was not a JSON object.".to_string())?;
    let tag_name = required_string(root, "tag_name")?;
    let name = optional_string(root, "name").unwrap_or_default();
    let html_uri = optional_string(root, "html_url");
    let published_at = match optional_string(root, "published_at") {
        Some(value) => {
            chrono::DateTime::parse_from_rfc3339(&value).map_err(|_| {
                "GitHub release response contains an invalid timestamp in published_at.".to_string()
            })?;
            Some(value)
        }
        None => None,
    };
    let draft = optional_bool(root, "draft");
    let prerelease = optional_bool(root, "prerelease");
    let assets = parse_assets(root);
    Ok(UpdateRelease {
        tag_name,
        name,
        html_uri,
        published_at,
        draft,
        prerelease,
        assets,
    })
}

fn parse_assets(root: &Map<String, Value>) -> Vec<UpdateAsset> {
    let Some(source_assets) = root.get("assets").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut assets = Vec::new();
    for source_asset in source_assets {
        let Some(item) = source_asset.as_object() else {
            continue;
        };
        let Some(asset_name) = optional_string(item, "name") else {
            continue;
        };
        let Some(download_uri) = optional_string(item, "browser_download_url") else {
            continue;
        };
        let size = item.get("size").and_then(Value::as_i64).unwrap_or(-1);
        let digest = optional_string(item, "digest");
        assets.push(UpdateAsset {
            name: asset_name,
            download_uri,
            size,
            digest,
        });
    }
    assets
}

fn required_string(object: &Map<String, Value>, name: &str) -> Result<String, String> {
    optional_string(object, name)
        .ok_or_else(|| format!("GitHub release response is missing {name}."))
}

fn optional_string(object: &Map<String, Value>, name: &str) -> Option<String> {
    let value = match object.get(name)? {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        _ => return None,
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn optional_bool(object: &Map<String, Value>, name: &str) -> bool {
    object.get(name).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_with_assets() {
        let body = r#"{
            "tag_name": "v2.3.0",
            "name": "korTTY v2.3.0",
            "html_url": "https://github.com/chardonnay/korTTY_rust/releases/tag/v2.3.0",
            "published_at": "2026-05-20T10:00:00Z",
            "draft": false,
            "prerelease": false,
            "assets": [
                {
                    "name": "korTTY-Windows-2.3.0-x86_64.msi",
                    "browser_download_url": "https://example.test/a.msi",
                    "size": 1234,
                    "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                },
                {
                    "name": "no-url-asset.zip"
                },
                {
                    "browser_download_url": "https://example.test/unnamed.zip"
                },
                "not-an-object"
            ]
        }"#;

        let release = parse_release(body).unwrap();

        assert_eq!(release.tag_name, "v2.3.0");
        assert_eq!(release.name, "korTTY v2.3.0");
        assert_eq!(
            release.html_uri.as_deref(),
            Some("https://github.com/chardonnay/korTTY_rust/releases/tag/v2.3.0")
        );
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-05-20T10:00:00Z")
        );
        assert!(release.is_stable_latest_release());
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "korTTY-Windows-2.3.0-x86_64.msi");
        assert_eq!(release.assets[0].size, 1234);
    }

    #[test]
    fn defaults_missing_optional_fields() {
        let release = parse_release(r#"{"tag_name": "v2.3.0"}"#).unwrap();

        assert_eq!(release.tag_name, "v2.3.0");
        assert_eq!(release.name, "");
        assert_eq!(release.html_uri, None);
        assert_eq!(release.published_at, None);
        assert!(!release.draft);
        assert!(!release.prerelease);
        assert!(release.assets.is_empty());
    }

    #[test]
    fn missing_asset_size_defaults_to_minus_one() {
        let release = parse_release(
            r#"{"tag_name": "v2.3.0", "assets": [{"name": "a.zip", "browser_download_url": "https://example.test/a.zip"}]}"#,
        )
        .unwrap();

        assert_eq!(release.assets[0].size, -1);
        assert_eq!(release.assets[0].digest, None);
    }

    #[test]
    fn rejects_missing_tag_name() {
        assert!(parse_release(r#"{"name": "no tag"}"#).is_err());
        assert!(parse_release(r#"{"tag_name": "   "}"#).is_err());
    }

    #[test]
    fn rejects_non_object_response_and_invalid_timestamp() {
        assert!(parse_release("[]").is_err());
        assert!(parse_release("not json").is_err());
        assert!(parse_release(r#"{"tag_name": "v2.3.0", "published_at": "yesterday"}"#).is_err());
    }
}
