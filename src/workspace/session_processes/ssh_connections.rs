use std::time::{Instant, SystemTime};

use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Element, Length};

use crate::session::{SessionId, Status};

use super::{Desktop, Dialog, Message, Workspace, duration};

pub(in crate::workspace) struct Connection {
    pub(in crate::workspace) session: SessionId,
    pub(in crate::workspace) endpoint: String,
    label: Option<String>,
    isolation: &'static str,
    monotonic: Instant,
    connected_at: SystemTime,
}

impl Workspace {
    pub(in crate::workspace) fn live_ssh_connections(&self) -> Vec<Connection> {
        let mut connections: Vec<_> = self
            .windows
            .values()
            .flat_map(|window| window.tabs.iter().filter_map(|entry| entry.terminal()))
            .filter(|tab| !tab.incognito)
            .flat_map(|tab| tab.grid.iter().map(move |(_, id)| (tab, *id)))
            .filter_map(|(tab, id)| {
                let pane = self.sessions.get(&id)?;
                if pane.control_ssh_provisional || pane.frame.status != Status::Running {
                    return None;
                }
                let ssh = pane.ssh.as_ref()?;
                let (monotonic, connected_at) = pane.session.ssh_connected_at()?;
                let isolation = match pane.isolation_name(&pane.frame.status) {
                    "sandboxed" => "Sandbox",
                    "process" => "Own process",
                    _ => "Application process",
                };
                let endpoint = ssh.label();
                let label = tab
                    .custom_title
                    .as_ref()
                    .filter(|label| **label != endpoint)
                    .cloned();
                Some(Connection {
                    session: id,
                    endpoint,
                    label,
                    isolation,
                    monotonic,
                    connected_at,
                })
            })
            .collect();
        connections.sort_unstable_by_key(|connection| (connection.monotonic, connection.session.0));
        connections
    }
}

pub(super) fn view<'a>(dialog: &'a Dialog, workspace: &'a Workspace) -> Element<'a, Desktop> {
    let connections = workspace.live_ssh_connections();
    let count = connections.len();
    let heading = row![
        text("Connection").width(Length::FillPortion(4)),
        text("Isolation").width(Length::FillPortion(2)),
        text("Connected at").width(Length::FillPortion(3)),
    ]
    .spacing(8);
    let mut list = column![].spacing(8).width(Length::Fill);
    for connection in connections {
        let endpoint = text(connection.endpoint).size(13);
        let identity = match connection.label {
            Some(label) => column![text(label), endpoint].spacing(2),
            None => column![endpoint],
        }
        .width(Length::FillPortion(4));
        let time = jiff::Timestamp::try_from(connection.connected_at).map_or_else(
            |_| "Unavailable".into(),
            |time| {
                time.to_zoned(dialog.zone.clone())
                    .strftime("%F %T")
                    .to_string()
            },
        );
        list = list.push(
            row![
                identity,
                text(connection.isolation)
                    .size(13)
                    .width(Length::FillPortion(2)),
                text(time).size(13).width(Length::FillPortion(3)),
            ]
            .spacing(8),
        );
    }
    let list: Element<'_, Desktop> = if count == 0 {
        container(text("No non-private SSH terminal connections are active."))
            .center(Length::Fill)
            .into()
    } else {
        scrollable(list).height(Length::Fill).into()
    };
    let application_memory = dialog.application_memory.map_or_else(
        || "Unavailable".into(),
        |bytes| format!("{:.0} MiB", bytes as f64 / 1_048_576.0),
    );
    column![
        text(format!("Active SSH connections · {count}")).size(18),
        text("Shows each live, non-private terminal connection.").size(13),
        heading,
        rule::horizontal(1),
        list,
        rule::horizontal(1),
        text(format!(
            "kortty {}  ·  Uptime {}  ·  Resident memory {}",
            env!("CARGO_PKG_VERSION"),
            duration(workspace.application_start.0.elapsed()),
            application_memory
        ))
        .size(13),
        row![
            space::horizontal(),
            button("Close").on_press(Desktop::SessionProcesses(dialog.id, Message::Close))
        ],
    ]
    .padding(12)
    .spacing(8)
    .height(Length::Fill)
    .into()
}
