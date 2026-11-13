//! Display-only progress for a still-unverified structured model response.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::{Deserializer as _, IgnoredAny, MapAccess, Visitor};
use zeroize::{Zeroize, Zeroizing};

use crate::local_ai::{StreamDelta, reply};
use crate::snippet_analysis::model::Analysis;
use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::mask;

const UPDATE_INTERVAL: Duration = Duration::from_millis(50);
const PREVIEW_BYTES: usize = 4096;

#[derive(Clone, Default)]
pub(crate) struct Preview {
    pub(super) answer_bytes: usize,
    pub(super) summary: String,
    pub(super) reasoning: String,
}

pub(super) fn reporter(
    redactor: Arc<Redactor>,
    send: async_channel::Sender<Preview>,
) -> impl FnMut(StreamDelta) + Send + 'static {
    let mut answer = Zeroizing::new(String::new());
    let mut reasoning = Zeroizing::new(String::new());
    let mut last = None;
    let mut summary = None;
    move |delta| {
        match delta {
            StreamDelta::Text(mut text) => {
                answer.push_str(&text);
                text.zeroize();
            }
            StreamDelta::Reasoning(mut text) => {
                reasoning.push_str(&text);
                text.zeroize();
            }
        }
        let now = Instant::now();
        if send.is_full() || last.is_some_and(|last| now.duration_since(last) < UPDATE_INTERVAL) {
            return;
        }
        if summary.is_none() && redactor.allows_incremental_output() {
            summary = complete_summary(&answer)
                .map(|value| bounded(mask::prompt(&redactor, &value).unwrap_or_default()));
        }
        let reasoning = bounded(mask::streaming(&redactor, &reasoning).unwrap_or_default());
        let preview = Preview {
            answer_bytes: answer.len(),
            summary: summary.clone().unwrap_or_default(),
            reasoning,
        };
        if send.try_send(preview).is_ok() {
            last = Some(now);
        }
    }
}

fn bounded(mut text: String) -> String {
    text.truncate(text.floor_char_boundary(PREVIEW_BYTES));
    text
}

/// Extract only a complete, decoded string field. Partial JSON and JSON escapes are never shown.
fn complete_summary(input: &str) -> Option<Zeroizing<String>> {
    struct Summary<'a>(&'a mut Option<Zeroizing<String>>);

    impl<'de> Visitor<'de> for Summary<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a snippet analysis object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<(), A::Error> {
            while let Some(key) = fields.next_key::<String>()? {
                if matches!(
                    key.as_str(),
                    "summary" | "explanation" | "description" | "overview"
                ) {
                    *self.0 = Some(Zeroizing::new(fields.next_value::<String>()?));
                    break;
                }
                fields.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }

    let mut summary = None;
    let mut decoder = serde_json::Deserializer::from_str(reply::strip_fence(input.trim_start()));
    // A closed string is displayable even while the outer object is incomplete.
    let _ = decoder.deserialize_map(Summary(&mut summary));
    summary
}

pub(super) fn mask_analysis(redactor: &Redactor, analysis: &mut Analysis) -> Result<(), String> {
    mask_fields(
        analysis,
        |text| mask::prompt_counted(redactor, text).map(|(masked, _)| masked),
        |value| Ok(!redactor.contains_known_secret(value)),
    )
}

pub(super) fn mask_cli_analysis(
    authority: &super::remote::CliAuthority,
    analysis: &mut Analysis,
) -> Result<(), String> {
    mask_fields(
        analysis,
        |text| authority.mask(text).map(|(masked, _)| masked),
        |value| authority.mask(value).map(|(masked, _)| masked == value),
    )
}

fn mask_fields(
    analysis: &mut Analysis,
    prose: impl Fn(&str) -> Result<String, String>,
    identity: impl Fn(&str) -> Result<bool, String>,
) -> Result<(), String> {
    let mask_prose = prose;
    let prose = std::iter::once(&mut analysis.summary)
        .chain(
            analysis
                .project_files
                .iter_mut()
                .map(|file| &mut file.purpose),
        )
        .chain(analysis.dependencies.iter_mut().flat_map(|item| {
            [
                &mut item.name,
                &mut item.kind,
                &mut item.purpose,
                &mut item.suggestion,
            ]
        }))
        .chain(analysis.improvements.iter_mut().flat_map(|item| {
            [
                &mut item.severity,
                &mut item.title,
                &mut item.detail,
                &mut item.recommendation,
            ]
        }));
    for text in prose {
        *text = mask_prose(text)?;
    }
    let identities = analysis
        .project_files
        .iter()
        .map(|file| file.path.as_str())
        .chain(analysis.dependencies.iter().map(|item| item.id.as_str()))
        .chain(
            analysis
                .improvements
                .iter()
                .flat_map(|item| std::iter::once(item.id.as_str()).chain(item.file.as_deref())),
        );
    for value in identities {
        if !identity(value)? {
            return Err(
                "The analysis contains sensitive text in a file or finding identifier.".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    #[test]
    fn provisional_summary_uses_complete_json_fields_and_masks_decoded_secrets() {
        assert!(complete_summary(r#"{"summary":"token prefi"#).is_none());
        let value =
            complete_summary(r#"{"summary":"pass\u002dphrase", "dependencies": ["#).unwrap();
        assert_eq!(value.as_str(), "pass-phrase");
        let secrets = Secrets::default();
        secrets.add("pass-phrase");
        let redactor = Arc::new(Redactor::for_journal(
            secrets.seal().unwrap(),
            Policy::default(),
        ));
        let (send, updates) = async_channel::bounded(1);
        reporter(redactor, send)(StreamDelta::Text(
            r#"{"summary":"pass\u002dphrase", "dependencies": ["#.into(),
        ));
        let preview = updates.try_recv().unwrap();
        assert_eq!(preview.summary, "***");
        assert!(preview.answer_bytes > preview.summary.len());
    }

    #[test]
    fn reviewed_snippet_analysis_displays_masked_stream_before_saving_and_cancels_its_socket() {
        use std::io::{ErrorKind, Read, Write};
        use std::sync::mpsc;
        use std::thread;

        use iced::futures::{FutureExt as _, StreamExt as _};
        use serde_json::json;

        use super::super::{
            Action, Configuration, Event, Message, SnippetMessage, SnippetWindow, State, Subject,
            Workspace, store,
        };
        use crate::local_ai::test_support::{accept, exclusive_run, listener, request};
        use crate::snippets::{Input, Library, tests::Profile};

        let _exclusive = exclusive_run();
        let profile = Profile::new();
        let snippet = Library::open(profile.path())
            .unwrap()
            .save(
                None,
                Input {
                    name: "Review script".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "echo verified\n".into(),
                },
            )
            .unwrap();
        let subject = Subject::Snippet(snippet);
        let window = SnippetWindow::open(profile.path()).unwrap();
        let history = store(&window).unwrap();
        let (listener, _) = listener();
        let config = serde_json::from_value::<Configuration>(json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "Local fixture", "endpoint": listener.local_addr().unwrap().to_string(), "model": "fixture-model"}],
                "default": 1, "last_id": 1
            }
        })).unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        workspace.window_settings.snippet_analysis_auto_diagram = false;
        let (owner, _) = workspace.open_window(false);
        let (id, _) = workspace.install_snippet_surface(owner, window, true);
        let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
        let next = |stream: &mut iced::futures::stream::BoxStream<
            'static,
            iced_runtime::Action<Message>,
        >| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match stream.next().now_or_never() {
                    Some(Some(iced_runtime::Action::Output(message))) => return message,
                    Some(None) => panic!("analysis task ended without an outcome"),
                    _ => {}
                }
                assert!(Instant::now() < deadline, "analysis task did not progress");
                thread::sleep(Duration::from_millis(5));
            }
        };
        let mut loaded =
            iced_runtime::task::into_stream(workspace.open_snippet_analysis(id, subject)).unwrap();
        let _ = workspace.update(next(&mut loaded));
        let _ = workspace.update_snippet_analysis(id, Action::RequestStart);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let (advance, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut socket = accept(&listener);
            let body = request(&mut socket);
            assert_eq!(body["stream"], true);
            assert_eq!(body["response_format"]["type"], "json_object");
            assert!(
                body["messages"][1]["content"]
                    .as_str()
                    .unwrap()
                    .contains("echo verified")
            );
            let header =
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            socket.get_mut().write_all(header).unwrap();
            let delta = |socket: &mut std::net::TcpStream, delta: serde_json::Value| {
                writeln!(
                    socket,
                    "data: {}\n",
                    json!({"choices":[{"index":0,"delta":delta}]})
                )
                .unwrap();
                socket.flush().unwrap();
            };
            delta(
                socket.get_mut(),
                json!({"reasoning_content":"Examining the command.\n"}),
            );
            wait.recv_timeout(Duration::from_secs(4)).unwrap();
            thread::sleep(UPDATE_INTERVAL);
            delta(
                socket.get_mut(),
                json!({"content":"{\"summary\":\"DB_PASSWORD=private-echo\","}),
            );
            wait.recv_timeout(Duration::from_secs(4)).unwrap();
            delta(
                socket.get_mut(),
                json!({"content":"\"improvements\":[],\"dependencies\":[]}"}),
            );
            socket.get_mut().write_all(b"data: [DONE]\n\n").unwrap();
            drop(socket);

            let mut socket = accept(&listener);
            let _ = request(&mut socket);
            socket.get_mut().write_all(header).unwrap();
            delta(
                socket.get_mut(),
                json!({"content":"{\"summary\":\"Canceled analysis\","}),
            );
            let mut byte = [0];
            let read = socket.read(&mut byte);
            assert!(
                matches!(read, Ok(0))
                    || read.is_err_and(|error| {
                        matches!(
                            error.kind(),
                            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                        )
                    })
            );
        });

        let start = workspace.update_snippet_analysis(id, Action::ConfirmStart);
        let mut outcomes = iced_runtime::task::into_stream(start).unwrap();
        let first = next(&mut outcomes);
        assert!(
            matches!(&first, Message::SnippetAnalysis(_, Event::Streaming(_, _, preview)) if preview.reasoning == "Examining the command.\n")
        );
        let _ = workspace.update(first);
        assert!(history.load(subject).unwrap().records.is_empty());
        advance.send(()).unwrap();
        let provisional = next(&mut outcomes);
        assert!(
            matches!(&provisional, Message::SnippetAnalysis(_, Event::Streaming(_, _, preview)) if preview.summary == "DB_PASSWORD=***")
        );
        let _ = workspace.update(provisional);
        let page = workspace.snippets[&id].analysis.page(subject).unwrap();
        assert!(
            matches!(&page.state, State::Requesting(run) if run.live.summary == "DB_PASSWORD=***")
        );
        assert!(history.load(subject).unwrap().records.is_empty());
        advance.send(()).unwrap();
        let saved = loop {
            let event = next(&mut outcomes);
            let final_reply = matches!(&event, Message::SnippetAnalysis(_, Event::Analyzed(..)));
            let task = workspace.update(event);
            if final_reply {
                break task;
            }
        };
        let mut saved = iced_runtime::task::into_stream(saved).unwrap();
        let _ = workspace.update(next(&mut saved));
        assert_eq!(
            history
                .load(subject)
                .unwrap()
                .current()
                .unwrap()
                .analysis
                .summary,
            "DB_PASSWORD=***"
        );

        let _ = workspace.update_snippet_analysis(id, Action::RequestStart);
        let mut canceled = iced_runtime::task::into_stream(
            workspace.update_snippet_analysis(id, Action::ConfirmStart),
        )
        .unwrap();
        let late = next(&mut canceled);
        assert!(matches!(
            &late,
            Message::SnippetAnalysis(_, Event::Streaming(..))
        ));
        let _ = workspace.update_snippet_analysis(id, Action::Abort);
        let _ = workspace.update(late);
        let _ = workspace.update(next(&mut canceled));
        assert_eq!(history.load(subject).unwrap().records.len(), 1);
        assert!(matches!(
            workspace.snippets[&id]
                .analysis
                .page(subject)
                .unwrap()
                .state,
            State::Ready
        ));
        server.join().unwrap();
    }
}
