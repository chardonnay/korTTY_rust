use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use kortty_terminal::ScreenSnapshot;
use regex::Regex;

use crate::session::ShellActivity;

const SETTLE: Duration = Duration::from_millis(250);
const TYPED_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Default)]
pub(super) struct Follow {
    pub(super) pinned: bool,
    pub(super) foreign: bool,
    directory_sequence: u64,
    command_sequence: u64,
    prompt_sequence: u64,
    report_host: Option<String>,
    path: Option<Vec<u8>>,
    pending: Option<(Instant, Vec<u8>)>,
}

impl Follow {
    pub(super) fn observe(
        &mut self,
        activity: &ShellActivity,
        screen: &ScreenSnapshot,
        user: &str,
        host: &str,
        now: Instant,
    ) {
        if screen.alternate_screen {
            self.pending = None;
            return;
        }
        let report = activity
            .directory
            .as_ref()
            .filter(|event| event.sequence != self.directory_sequence);
        let typed = activity
            .typed_cd
            .as_ref()
            .filter(|event| event.sequence != self.command_sequence);
        let new_prompt = activity.prompt != self.prompt_sequence;
        if report.is_none() && typed.is_none() && !new_prompt && !self.foreign {
            return;
        }
        self.prompt_sequence = activity.prompt;
        let verdict = identity(screen, user, self.report_host.as_deref().unwrap_or(host));
        if verdict == Identity::Foreign {
            self.foreign = true;
            self.pending = None;
        } else if verdict == Identity::Native && report.is_none() && new_prompt && self.foreign {
            self.foreign = false;
            if !self.pinned {
                self.pending = self.path.clone().map(|path| (now + SETTLE, path));
            }
        }

        if let Some(report) = report {
            self.directory_sequence = report.sequence;
            if !report.host.is_empty()
                && self
                    .report_host
                    .as_ref()
                    .is_some_and(|known| !same_host(&report.host, known))
            {
                self.foreign = true;
                self.pending = None;
            } else if verdict != Identity::Foreign
                && (!report.host.is_empty() || verdict == Identity::Native)
            {
                if !report.host.is_empty() && self.report_host.is_none() {
                    // A login address can be a DNS alias or an IP. The first non-foreign
                    // report anchors the shell label; SFTP never opens another host.
                    self.report_host = Some(report.host.to_string());
                }
                self.foreign = false;
                self.report_path(report.path.as_bytes().to_vec(), now);
            }
        }
        if let Some(typed) = typed {
            self.command_sequence = typed.sequence;
            if !self.foreign
                && verdict != Identity::Foreign
                && self.directory_sequence <= typed.sequence
                && now.saturating_duration_since(typed.submitted_at) <= TYPED_WINDOW
                && activity.prompt > typed.prompt_before
                && activity.output > typed.output_before
                && activity
                    .command_exit
                    .as_ref()
                    .is_some_and(|exit| exit.sequence > typed.sequence && exit.status == Some(0))
            {
                let path = typed.path.as_bytes();
                let path = if path.starts_with(b"/") {
                    Some(path.to_vec())
                } else {
                    self.path.as_ref().map(|root| join(root, path))
                };
                if let Some(path) = path {
                    self.report_path(path, now);
                }
            }
        }
    }

    fn report_path(&mut self, path: Vec<u8>, now: Instant) {
        if path.len() > 4096 || self.path.as_ref() == Some(&path) {
            return;
        }
        self.path = Some(path.clone());
        if !self.pinned {
            self.pending = Some((now + SETTLE, path));
        }
    }

    pub(super) fn initial_path(&mut self) -> Option<Vec<u8>> {
        if self.foreign || self.pinned {
            return None;
        }
        self.pending.take();
        self.path.clone()
    }

    pub(super) fn shell_path(&self) -> Option<&[u8]> {
        if self.foreign {
            return None;
        }
        self.path.as_deref()
    }

    pub(super) fn toggle_pin(&mut self, now: Instant) {
        self.pinned = !self.pinned;
        self.pending = if !self.pinned && !self.foreign {
            self.path.clone().map(|path| (now, path))
        } else {
            None
        };
    }

    pub(super) fn cancel_listing(&mut self) {
        self.pending = None;
    }

    pub(super) fn awaiting_listing(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn ready_path(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.pending
            .take_if(|(due, _)| *due <= now)
            .map(|(_, path)| path)
    }
}

#[derive(PartialEq, Eq)]
enum Identity {
    Native,
    Foreign,
    OtherHost,
    Unknown,
}

pub(super) fn file_context(
    activity: &ShellActivity,
    screen: &ScreenSnapshot,
    user: &str,
    host: &str,
) -> Option<Option<Vec<u8>>> {
    let verdict = identity(screen, user, host);
    if matches!(verdict, Identity::Foreign | Identity::OtherHost) {
        return None;
    }
    let Some(report) = activity.directory.as_ref() else {
        return Some(None);
    };
    if !report.host.is_empty() && !same_host(&report.host, host) {
        return None;
    }
    Some(
        (!report.host.is_empty() || verdict == Identity::Native)
            .then(|| report.path.as_bytes().to_vec()),
    )
}

fn identity(screen: &ScreenSnapshot, user: &str, host: &str) -> Identity {
    static PROMPT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:\([^)]*\)\s*)?\[?([A-Za-z_][A-Za-z0-9._-]*)@([A-Za-z0-9][A-Za-z0-9.-]*)")
            .expect("valid prompt pattern")
    });
    let Some(line) = (0..screen.rows)
        .rev()
        .filter_map(|row| screen.row(row))
        .find_map(|cells| {
            let line = cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>();
            (!line.trim().is_empty()).then_some(line)
        })
    else {
        return Identity::Unknown;
    };
    let line = line.trim();
    if !line.ends_with(['$', '#', '%', '>']) {
        return Identity::Unknown;
    }
    if let Some(captures) = PROMPT.captures(line) {
        if !captures[1].eq_ignore_ascii_case(user) {
            Identity::Foreign
        } else if same_host(&captures[2], host) {
            Identity::Native
        } else {
            Identity::OtherHost
        }
    } else if line.ends_with('#') && !user.eq_ignore_ascii_case("root") {
        Identity::Foreign
    } else {
        Identity::Unknown
    }
}

fn same_host(reported: &str, expected: &str) -> bool {
    let reported = reported.trim_matches(['[', ']']);
    let expected = expected.trim_matches(['[', ']']);
    match (reported.parse::<IpAddr>(), expected.parse::<IpAddr>()) {
        (Ok(a), Ok(b)) => a == b,
        (Err(_), Err(_)) => {
            let a = reported.trim_end_matches('.');
            let b = expected.trim_end_matches('.');
            a.eq_ignore_ascii_case(b)
                || (!a.contains('.')
                    && b.split('.')
                        .next()
                        .is_some_and(|label| label.eq_ignore_ascii_case(a)))
                || (!b.contains('.')
                    && a.split('.')
                        .next()
                        .is_some_and(|label| label.eq_ignore_ascii_case(b)))
        }
        _ => false,
    }
}

pub(super) fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(parent.len() + name.len() + 1);
    path.extend_from_slice(parent);
    if !parent.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::session::ReportedDirectory;

    use super::*;

    #[test]
    fn terminal_file_context_rejects_same_user_on_a_different_host() {
        let mut terminal = kortty_terminal::Terminal::new(80, 24);
        terminal.advance(b"operator@nested-host:~$ ");
        let activity = ShellActivity {
            directory: Some(ReportedDirectory {
                sequence: 1,
                host: Arc::from("nested-host"),
                path: Arc::from("/wrong"),
            }),
            ..Default::default()
        };
        assert!(file_context(&activity, &terminal.snapshot(), "operator", "outer-host").is_none());
        assert_eq!(
            file_context(
                &activity,
                &terminal.snapshot(),
                "operator",
                "nested-host.example"
            ),
            Some(Some(b"/wrong".to_vec())),
        );
        let mut terminal = kortty_terminal::Terminal::new(80, 24);
        terminal.advance(b"$ ");
        assert!(file_context(&activity, &terminal.snapshot(), "operator", "outer-host").is_none());
        assert_eq!(
            file_context(
                &ShellActivity::default(),
                &terminal.snapshot(),
                "operator",
                "outer-host"
            ),
            Some(None)
        );
    }

    #[test]
    fn remote_sidebar_debounces_an_aliased_host_and_respects_foreign_and_pinned_paths() {
        let mut terminal = kortty_terminal::Terminal::new(80, 24);
        terminal.advance(b"operator@actual-node:~$ ");
        let screen = terminal.snapshot();
        let now = Instant::now();
        let mut follow = Follow::default();
        let report = |sequence, host: &str, path: &str| ShellActivity {
            directory: Some(ReportedDirectory {
                sequence,
                host: Arc::from(host),
                path: Arc::from(path),
            }),
            ..Default::default()
        };
        follow.observe(
            &report(1, "actual-node", "/first"),
            &screen,
            "operator",
            "192.0.2.4",
            now,
        );
        follow.observe(
            &report(2, "actual-node", "/latest"),
            &screen,
            "operator",
            "192.0.2.4",
            now,
        );
        assert!(!follow.foreign);
        assert!(follow.ready_path(now).is_none());
        assert_eq!(
            follow.ready_path(now + SETTLE).as_deref(),
            Some(b"/latest".as_slice())
        );

        follow.observe(
            &report(3, "another-node", "/wrong"),
            &screen,
            "operator",
            "192.0.2.4",
            now,
        );
        assert!(follow.foreign);
        assert!(follow.shell_path().is_none());
        assert!(follow.ready_path(now + SETTLE).is_none());

        follow.toggle_pin(now);
        follow.observe(
            &report(4, "actual-node.example", "/resumed"),
            &screen,
            "operator",
            "192.0.2.4",
            now,
        );
        assert!(!follow.foreign);
        assert_eq!(follow.shell_path(), Some(b"/resumed".as_slice()));
        assert!(follow.ready_path(now + SETTLE).is_none());
        follow.toggle_pin(now);
        assert_eq!(
            follow.ready_path(now).as_deref(),
            Some(b"/resumed".as_slice())
        );
    }
}
