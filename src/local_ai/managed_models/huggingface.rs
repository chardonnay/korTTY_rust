//! Explicit Hugging Face GGUF search and immutable artifact metadata.

mod multipart;
mod recommendations;
mod transfer;

use std::fmt;
use std::io::{self, Read as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kortty_vault::Secret;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ureq::{
    Agent, Body,
    http::{HeaderValue, Response},
};
use url::Url;

use super::{MAX_MODEL_BYTES, Purpose};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::runtime_package::{Pending, work_with_agent};

pub(crate) use multipart::Selection;
pub(crate) use recommendations::{GIB, Recommendation, RecommendationRole, recommendations};
pub(crate) use transfer::{Downloaded, Pause, Progress, discard_paused, download, paused};

const HUB: &str = "https://huggingface.co/";
const SEARCH_LIMIT: usize = 60;
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_REDIRECTS: usize = 5;
const MAX_LICENSE_BYTES: usize = 80;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct Repository(String);

impl Repository {
    fn parse(value: &str) -> Result<Self, String> {
        let mut segments = value.split('/');
        if value.len() <= 192
            && segments.next().is_some_and(repository_segment)
            && segments.next().is_some_and(repository_segment)
            && segments.next().is_none()
        {
            Ok(Self(value.into()))
        } else {
            Err("The Hub returned an invalid model repository identifier.".into())
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Artifact {
    path: String,
    size: u64,
    digest: String,
}

impl Artifact {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    fn valid(&self) -> bool {
        valid_file_path(&self.path)
            && (1..=MAX_MODEL_BYTES).contains(&self.size)
            && lowercase_hex(&self.digest, 64)
    }
}

impl fmt::Display for Artifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} · {} MiB",
            self.path,
            self.size.div_ceil(1024 * 1024)
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Review {
    pub(crate) repository: Repository,
    pub(crate) revision: String,
    pub(crate) files: Vec<Selection>,
    license: Option<String>,
}

impl Review {
    pub(crate) fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    pub(crate) fn select(&self, selection: &Selection, purpose: Purpose) -> Result<Plan, String> {
        if !self.files.contains(selection) {
            return Err("Review the selected Hub file again.".into());
        }
        let plan = Plan {
            repository: self.repository.clone(),
            revision: self.revision.clone(),
            file: selection.files()[0].clone(),
            additional: selection.files()[1..].to_vec(),
            purpose,
        };
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub(crate) repository: Repository,
    pub(crate) revision: String,
    pub(crate) file: Artifact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    additional: Vec<Artifact>,
    pub(crate) purpose: Purpose,
}

impl Plan {
    fn validate(&self) -> Result<(), String> {
        Repository::parse(self.repository.id())?;
        if !lowercase_hex(&self.revision, 40)
            || self.count() > super::split::MAX_SHARDS
            || self.files().any(|file| !file.valid())
            || !multipart::complete(self.files())
            || multipart::total(self.files()).is_none()
        {
            return Err(
                "The Hub artifact needs an immutable revision, size and SHA-256 digest.".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn destination(&self, file: &Artifact) -> String {
        artifact_url(&origin(), self, file).into()
    }

    pub(crate) fn files(&self) -> impl Iterator<Item = &Artifact> {
        std::iter::once(&self.file).chain(&self.additional)
    }

    pub(crate) fn count(&self) -> usize {
        1 + self.additional.len()
    }

    pub(crate) fn total(&self) -> u64 {
        multipart::total(self.files()).expect("validated Hub download size")
    }
}

pub(crate) fn search(
    query: String,
    token: Option<Secret>,
) -> Result<Pending<Vec<Repository>>, String> {
    let query = query.trim();
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err("Enter a model search of at most 256 bytes.".into());
    }
    let query = query.to_owned();
    let hub = Hub::new(token, Duration::from_secs(30))?;
    work_with_agent(Some(hub.agent.clone()), move |cancelled| {
        hub.search(&query, cancelled)
    })
}

pub(crate) fn review(
    repository: Repository,
    token: Option<Secret>,
) -> Result<Pending<Arc<Review>>, String> {
    Repository::parse(repository.id())?;
    let hub = Hub::new(token, Duration::from_secs(30))?;
    work_with_agent(Some(hub.agent.clone()), move |cancelled| {
        hub.review(repository, None, cancelled).map(Arc::new)
    })
}

impl Recommendation {
    pub(crate) fn review(
        self: &Arc<Self>,
        token: Option<Secret>,
    ) -> Result<Pending<Arc<Review>>, String> {
        let repository = Repository::parse(&self.model_id)?;
        let hub = Hub::new(token, Duration::from_secs(30))?;
        let recommendation = Arc::clone(self);
        work_with_agent(Some(hub.agent.clone()), move |cancelled| {
            let mut review =
                hub.review(repository, recommendation.revision.as_deref(), cancelled)?;
            recommendation.retain_files(&mut review)?;
            Ok(Arc::new(review))
        })
    }
}

struct Hub {
    agent: Agent,
    origin: Url,
    authorization: Option<HeaderValue>,
    authorize_downloads: Box<dyn Fn() -> io::Result<()> + Send + Sync>,
    timeout: Duration,
}

impl Hub {
    fn new(token: Option<Secret>, timeout: Duration) -> Result<Self, String> {
        let authorization = token
            .as_deref()
            .map(|value| {
                if value.is_empty()
                    || value.len() > 4096
                    || !value.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err("The selected Hugging Face token is invalid.".into());
                }
                super::super::bearer_header(value)
                    .map_err(|_| "The selected Hugging Face token is invalid.".to_owned())
            })
            .transpose()?;
        Ok(Self {
            agent: Agent::new_with_config(super::super::connection_config(timeout)),
            origin: origin(),
            authorization,
            authorize_downloads: Box::new(|| ClipboardPolicy::load().require_ai_model_downloads()),
            timeout,
        })
    }

    fn require_downloads(&self) -> io::Result<()> {
        (self.authorize_downloads)()
    }

    fn search(&self, query: &str, cancelled: &AtomicBool) -> Result<Vec<Repository>, String> {
        let mut url = api_url(&self.origin, &[]);
        url.query_pairs_mut()
            .append_pair("filter", "gguf")
            .append_pair("limit", &SEARCH_LIMIT.to_string())
            .append_pair("sort", "downloads")
            .append_pair("direction", "-1");
        if !query.is_empty() {
            url.query_pairs_mut().append_pair("search", query);
        }
        let wire: Vec<RepositoryWire> = self.json(url, cancelled)?;
        if wire.len() > SEARCH_LIMIT {
            return Err("The Hub returned too many search results.".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        Ok(wire
            .into_iter()
            .filter_map(|item| item.id().and_then(|id| Repository::parse(id).ok()))
            .filter(|repository| seen.insert(repository.id().to_ascii_lowercase()))
            .collect())
    }

    fn review(
        &self,
        repository: Repository,
        expected_revision: Option<&str>,
        cancelled: &AtomicBool,
    ) -> Result<Review, String> {
        let mut segments: Vec<_> = repository.id().split('/').collect();
        if let Some(revision) = expected_revision {
            if !lowercase_hex(revision, 40) {
                return Err("The model recommendation requires an immutable commit.".into());
            }
            segments.extend(["revision", revision]);
        }
        let mut url = api_url(&self.origin, &segments);
        url.query_pairs_mut().append_pair("blobs", "true");
        let wire: ModelWire = self.json(url, cancelled)?;
        let review = parse_review(repository, wire)?;
        if expected_revision.is_some_and(|revision| revision != review.revision) {
            return Err("The Hub metadata did not match the recommended commit.".into());
        }
        Ok(review)
    }

    fn json<T: DeserializeOwned>(&self, url: Url, cancelled: &AtomicBool) -> Result<T, String> {
        let mut response = self.get(url, None, false, None, cancelled)?;
        if response.status().as_u16() != 200 {
            return Err(status_message(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| network_error(cancelled))?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            return Err("Hugging Face metadata exceeds the response limit.".into());
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(network_error(cancelled));
        }
        self.require_downloads()
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&bytes)
            .map_err(|_| "The Hub returned invalid model metadata.".into())
    }

    fn artifact(
        &self,
        plan: &Plan,
        file: &Artifact,
        range: Option<Range<'_>>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Response<Body>, String> {
        self.get(
            artifact_url(&self.origin, plan, file),
            range,
            true,
            Some(deadline),
            cancelled,
        )
    }

    fn get(
        &self,
        mut url: Url,
        range: Option<Range<'_>>,
        artifact: bool,
        deadline: Option<Instant>,
        cancelled: &AtomicBool,
    ) -> Result<Response<Body>, String> {
        let mut send_token = true;
        for hops in 0..=MAX_REDIRECTS {
            if cancelled.load(Ordering::Acquire) {
                return Err(network_error(cancelled));
            }
            self.require_downloads()
                .map_err(|error| error.to_string())?;
            let mut request = self
                .agent
                .get(url.as_str())
                .header("Accept-Encoding", "identity")
                .header("User-Agent", "kortty-huggingface")
                .header(
                    "Accept",
                    if artifact {
                        "application/octet-stream"
                    } else {
                        "application/json"
                    },
                );
            if send_token
                && url.origin() == self.origin.origin()
                && let Some(header) = &self.authorization
            {
                request = request.header("Authorization", header.clone());
            }
            if let Some(range) = range {
                request = request
                    .header("Range", format!("bytes={}-", range.start))
                    .header("If-Range", range.etag);
            }
            let config = request.config().http_status_as_error(false);
            let config = if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("The model transfer exceeded its time limit.".into());
                }
                config.timeout_global(Some(remaining.min(self.timeout)))
            } else {
                config
            };
            let response = config
                .build()
                .call()
                .map_err(|_| network_error(cancelled))?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            if hops == MAX_REDIRECTS {
                return Err("Hugging Face redirected too many times.".into());
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 16 * 1024)
                .ok_or("Hugging Face returned an invalid redirect.")?;
            let next = url
                .join(location)
                .map_err(|_| "Hugging Face returned an invalid redirect.")?;
            if !allowed_destination(&self.origin, &next, artifact) {
                return Err(
                    "The Hub redirected outside the reviewed download destinations.".into(),
                );
            }
            send_token &= next.origin() == self.origin.origin();
            url = next;
        }
        unreachable!("redirect loop is bounded")
    }
}

#[derive(Clone, Copy)]
struct Range<'a> {
    start: u64,
    etag: &'a str,
}

fn origin() -> Url {
    Url::parse(HUB).expect("fixed Hugging Face origin")
}

fn api_url(origin: &Url, segments: &[&str]) -> Url {
    let mut url = origin.clone();
    url.path_segments_mut()
        .expect("Hub has path segments")
        .clear()
        .extend(["api", "models"])
        .extend(segments.iter().copied());
    url
}

fn artifact_url(origin: &Url, plan: &Plan, file: &Artifact) -> Url {
    let mut url = origin.clone();
    url.path_segments_mut()
        .expect("Hub has path segments")
        .clear()
        .extend(plan.repository.id().split('/'))
        .extend(["resolve", &plan.revision])
        .extend(file.path.split('/'));
    url
}

fn allowed_destination(origin: &Url, next: &Url, artifact: bool) -> bool {
    if !next.username().is_empty() || next.password().is_some() || next.fragment().is_some() {
        return false;
    }
    if next.origin() == origin.origin() {
        return artifact || next.path() == "/api/models" || next.path().starts_with("/api/models/");
    }
    artifact
        && next.scheme() == "https"
        && next.port().is_none()
        && matches!(
            next.host_str(),
            Some(
                "cas-bridge.xethub.hf.co"
                    | "cas-bridge.xethub-eu.hf.co"
                    | "cas-server.xethub.hf.co"
                    | "cas-server.xethub-eu.hf.co"
                    | "transfer.xethub.hf.co"
                    | "transfer.xethub-eu.hf.co"
                    | "us.aws.cdn.hf.co"
                    | "us.gcp.cdn.hf.co"
                    | "cdn-lfs.huggingface.co"
                    | "cdn-lfs-us-1.hf.co"
                    | "cdn-lfs-eu-1.hf.co"
            )
        )
}

#[derive(Deserialize)]
struct RepositoryWire {
    id: Option<String>,
    #[serde(rename = "modelId")]
    model_id: Option<String>,
}

impl RepositoryWire {
    fn id(&self) -> Option<&str> {
        self.id.as_deref().or(self.model_id.as_deref())
    }
}

#[derive(Deserialize)]
struct ModelWire {
    #[serde(flatten)]
    repository: RepositoryWire,
    sha: Option<String>,
    #[serde(default)]
    siblings: Vec<Sibling>,
    #[serde(rename = "cardData")]
    card_data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Sibling {
    rfilename: String,
    size: Option<u64>,
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    size: Option<u64>,
    sha256: Option<String>,
    oid: Option<String>,
}

fn parse_review(repository: Repository, wire: ModelWire) -> Result<Review, String> {
    if wire.repository.id() != Some(repository.id()) {
        return Err("The Hub metadata refers to a different repository.".into());
    }
    let revision = wire
        .sha
        .map(|sha| sha.to_ascii_lowercase())
        .filter(|sha| lowercase_hex(sha, 40))
        .ok_or("The Hub did not report an immutable commit for this repository.")?;
    // Model-card metadata is descriptive. The download plan only uses the artifact metadata.
    let license = wire
        .card_data
        .as_ref()
        .and_then(|card| card.get("license"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            value.len() <= MAX_LICENSE_BYTES
                && value
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+')
                })
        })
        .map(str::to_owned);
    let mut files = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for sibling in wire.siblings {
        let Some(lfs) = sibling.lfs else {
            continue;
        };
        let Some(size) = lfs.size.or(sibling.size) else {
            continue;
        };
        if sibling.size.is_some_and(|other| other != size) {
            continue;
        }
        let Some(digest) = lfs.sha256.or(lfs.oid) else {
            continue;
        };
        let digest = digest
            .strip_prefix("sha256:")
            .unwrap_or(&digest)
            .to_ascii_lowercase();
        let file = Artifact {
            path: sibling.rfilename,
            size,
            digest,
        };
        if file.valid() {
            if files.len() == MAX_FILES || !seen.insert(file.path.clone()) {
                return Err("The Hub returned too many files or duplicate file metadata.".into());
            }
            files.push(file);
        }
    }
    Ok(Review {
        repository,
        revision,
        files: multipart::selections(files),
        license,
    })
}

fn repository_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_file_path(path: &str) -> bool {
    if path.len() > 1024
        || path.split('/').count() > 12
        || path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || part.chars().any(|character| {
                    character.is_control()
                        || !crate::session_journal::is_display_character(character)
                        || matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
    {
        return false;
    }
    let leaf = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(stem) = leaf.strip_suffix(".gguf") else {
        return false;
    };
    let shard = stem.rsplit_once("-of-").is_some_and(|(prefix, count)| {
        count.bytes().all(|byte| byte.is_ascii_digit())
            && prefix
                .rsplit_once('-')
                .is_some_and(|(_, number)| number.bytes().all(|byte| byte.is_ascii_digit()))
    });
    !stem.is_empty()
        && !stem.contains("mmproj")
        && (!shard
            || super::split::components(path).is_some_and(|(_, index, count)| {
                (2..=super::split::MAX_SHARDS).contains(&count) && (1..=count).contains(&index)
            }))
}

fn status_message(status: u16) -> String {
    match status {
        401 | 403 => format!(
            "Hugging Face returned HTTP {status}. Check access to this repository and the selected token."
        ),
        _ => format!("Hugging Face returned HTTP {status}."),
    }
}

fn network_error(cancelled: &AtomicBool) -> String {
    if cancelled.load(Ordering::Acquire) {
        "Hugging Face operation canceled.".into()
    } else {
        "Could not read the Hugging Face response.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ai_model_download_policy_rechecks_before_following_a_hub_redirect() {
        use std::io::Write as _;

        use crate::local_ai::test_support::{
            accept, fixture_http_response, listener, wire_request,
        };

        let (server, _) = listener();
        let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
        hub.origin = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let permit = Arc::clone(&allowed);
        hub.authorize_downloads = Box::new(move || {
            if permit.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Fixture policy denied downloads",
                ))
            }
        });
        let worker = std::thread::spawn(move || {
            let mut first = accept(&server);
            wire_request(
                &mut first,
                "GET /api/models?filter=gguf&limit=60&sort=downloads&direction=-1 HTTP/1.1",
            );
            allowed.store(false, Ordering::Release);
            first
                .get_mut()
                .write_all(
                    fixture_http_response("302 Found", "Location: /api/models/review\r\n", "")
                        .as_bytes(),
                )
                .unwrap();
            server
        });
        let error = hub.search("", &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error, "Fixture policy denied downloads");
        let server = worker.join().unwrap();
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn review_requires_an_immutable_commit_and_complete_gguf_metadata() {
        let repository = Repository::parse("example/fixture").unwrap();
        let digest = "a".repeat(64);
        let revision = "b".repeat(40);
        let wire = json!({"id":repository.id(), "modelId": repository.id(), "sha": revision, "siblings":[
            {"rfilename":"folder/Größe.gguf", "size":4000, "lfs":{"size":4000,"sha256":digest}},
            {"rfilename":"../unsafe.gguf", "lfs":{"size":10,"sha256":digest}},
            {"rfilename":"model-00001-of-00002.gguf", "lfs":{"size":10,"sha256":digest}},
            {"rfilename":"mmproj-fixture.gguf", "lfs":{"size":10,"sha256":digest}},
            {"rfilename":"missing-hash.gguf", "lfs":{"size":10}},
            {"rfilename":"mismatch.gguf", "size":9, "lfs":{"size":10,"sha256":digest}}
        ]});
        let review = parse_review(
            repository.clone(),
            serde_json::from_value(wire.clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(review.files.len(), 1);
        let plan = review.select(&review.files[0], Purpose::Chat).unwrap();
        assert!(
            plan.destination(&plan.file)
                .ends_with(&format!("/resolve/{revision}/folder/Gr%C3%B6%C3%9Fe.gguf"))
        );
        let mut bad = wire;
        bad["sha"] = "main".into();
        assert!(parse_review(repository, serde_json::from_value(bad).unwrap()).is_err());
    }

    #[test]
    fn hub_license_metadata_is_bounded_and_does_not_change_the_download_plan() {
        let repository = Repository::parse("example/fixture").unwrap();
        let wire = json!({
            "id": repository.id(),
            "sha": "b".repeat(40),
            "siblings": [{"rfilename": "model.gguf", "lfs": {"size": 4, "sha256": "a".repeat(64)}}]
        });
        let review =
            |wire| parse_review(repository.clone(), serde_json::from_value(wire).unwrap()).unwrap();
        let no_card = review(wire.clone());
        let exact = no_card.select(&no_card.files[0], Purpose::Chat).unwrap();
        assert_eq!(no_card.license(), None);
        let mut licensed = wire.clone();
        licensed["cardData"] =
            json!({"license": "apache-2.0", "license_link": "https://fixtures.example.test"});
        let licensed = review(licensed);
        assert_eq!(licensed.license(), Some("apache-2.0"));
        assert_eq!(
            licensed.select(&licensed.files[0], Purpose::Chat).unwrap(),
            exact
        );
        for invalid in [
            json!(42),
            json!({"license": ["mit"]}),
            json!({"license": "https://fixtures.example.test"}),
            json!({"license": "mit\u{202e}"}),
            json!({"license": "mit\nother"}),
            json!({"license": "x".repeat(MAX_LICENSE_BYTES + 1)}),
        ] {
            let mut malformed = wire.clone();
            malformed["cardData"] = invalid;
            let malformed = review(malformed);
            assert_eq!(malformed.license(), None);
            assert_eq!(
                malformed
                    .select(&malformed.files[0], Purpose::Chat)
                    .unwrap(),
                exact
            );
        }
    }

    #[test]
    fn pinned_model_review_binds_the_exact_metadata_url_commit_and_quantization() {
        use std::io::Write as _;

        use crate::local_ai::test_support::{
            accept, fixture_http_response, listener, wire_request,
        };

        let candidates = recommendations(None, RecommendationRole::Coding, Some(16 * GIB));
        let recommendation = &candidates[0];
        let repository = Repository::parse(&recommendation.model_id).unwrap();
        let revision = recommendation.revision.as_deref().unwrap();
        let digest = "a".repeat(64);
        let file = |rfilename| json!({"rfilename": rfilename, "lfs":{"size":4,"sha256":digest}});
        let wire = json!({"id":repository.id(), "sha": revision, "cardData":{"license":"apache-2.0"}, "siblings":[
            file("shards/model-q4_k_m-00001-of-00002.gguf"),
            file("shards/model-q4_k_m-00002-of-00002.gguf"),
            file("model.Q4_K_M.gguf"),
            file("model-q4_k_m_extra.gguf"),
            file("model-q8_0.gguf"),
            file("broken/model-q4_k_m-00001-of-00002.gguf"),
        ]});
        let (server, _) = listener();
        let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
        hub.origin = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
        hub.authorize_downloads = Box::new(|| Ok(()));
        let expected = format!(
            "GET /api/models/{}/revision/{revision}?blobs=true HTTP/1.1",
            repository.id()
        );
        let wrong_revision = "b".repeat(40);
        let worker = std::thread::spawn(move || {
            let mut wrong = wire.clone();
            wrong["sha"] = wrong_revision.into();
            for body in [wrong, wire] {
                let mut stream = accept(&server);
                wire_request(&mut stream, &expected);
                stream
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
                    .unwrap();
            }
        });
        let cancelled = AtomicBool::new(false);
        assert_eq!(
            hub.review(repository.clone(), Some(revision), &cancelled)
                .unwrap_err(),
            "The Hub metadata did not match the recommended commit."
        );
        let mut review = hub.review(repository, Some(revision), &cancelled).unwrap();
        recommendation.retain_files(&mut review).unwrap();
        assert!(recommendation.matches_review(&review));
        assert_eq!(review.license(), Some("apache-2.0"));
        assert_eq!(review.files.len(), 2);
        let plan = review
            .select(&review.files[0], RecommendationRole::Coding.purpose())
            .unwrap();
        assert_eq!(plan.count(), 1);
        assert_eq!(plan.file.path(), "model.Q4_K_M.gguf");
        assert!(
            plan.destination(&plan.file)
                .ends_with(&format!("/resolve/{revision}/model.Q4_K_M.gguf"))
        );
        assert_eq!(review.files[1].files().len(), 2);
        worker.join().unwrap();
    }

    #[test]
    fn redirects_keep_metadata_on_the_hub_and_allow_only_approved_artifact_hosts() {
        let hub = origin();
        let cdn = Url::parse("https://cas-bridge.xethub.hf.co/blob?signature=fixture").unwrap();
        assert!(allowed_destination(&hub, &cdn, true));
        assert!(!allowed_destination(&hub, &cdn, false));
        for invalid in [
            "https://cas-bridge.xethub.hf.co.example.test/blob",
            "http://cas-bridge.xethub.hf.co/blob",
            "https://user@cas-bridge.xethub.hf.co/blob",
            "https://cas-bridge.xethub.hf.co:8443/blob",
        ] {
            assert!(!allowed_destination(
                &hub,
                &Url::parse(invalid).unwrap(),
                true
            ));
        }
    }
}
