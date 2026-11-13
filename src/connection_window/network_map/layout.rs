use std::cmp::Reverse;
use std::collections::HashMap;

use iced::{Color, Point, Rectangle, Size};

use crate::connection_store::teamwork;
use crate::connection_store::{Connection, TunnelTarget};

use super::super::{ConnectionWindow, RowKey, Scope, os_badge, selection::VisualRow};

const NODE_WIDTH: f32 = 176.0;
const NODE_HEIGHT: f32 = 44.0;
const NODE_GAP_X: f32 = 40.0;
const NODE_GAP_Y: f32 = 22.0;
const ZONE_PADDING: f32 = 14.0;
const ZONE_TITLE: f32 = 28.0;
const ZONE_GAP: f32 = 34.0;
const ORIGIN_X: f32 = 270.0;
const ORIGIN_Y: f32 = 24.0;
const COMPUTER: usize = 0;

pub(super) struct Layout {
    pub(super) zones: Vec<Zone>,
    pub(super) nodes: Vec<Node>,
    pub(super) edges: Vec<Edge>,
    pub(super) size: Size,
}

/// The visible map data, independent of the window and the profile store.
pub(crate) struct Topology {
    pub(crate) scope: String,
    pub(crate) connection_count: usize,
    nodes: Vec<Node>,
    members: Vec<Members>,
    edges: Vec<Edge>,
}

pub(super) struct Zone {
    pub(super) bounds: Rectangle,
    pub(super) group: Option<String>,
    pub(super) title: String,
    pub(super) color: Option<Color>,
    pub(super) drop_target: bool,
    pub(super) teamwork: bool,
}

#[derive(Clone)]
pub(super) struct Node {
    pub(super) bounds: Rectangle,
    pub(super) kind: NodeKind,
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) badge: Option<super::pdf::NodeBadge>,
}

impl Node {
    pub(super) fn is_shared(&self) -> bool {
        matches!(self.kind, NodeKind::Saved(RowKey::Shared(_)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum NodeKind {
    Computer,
    Saved(RowKey),
    Jump,
}

#[derive(Clone)]
pub(super) struct Edge {
    pub(super) from: usize,
    pub(super) to: usize,
    pub(super) kind: EdgeKind,
    pub(super) label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EdgeKind {
    Jump,
    Tunnel,
}

struct Members {
    group: Option<String>,
    title: String,
    color: Option<Color>,
    external: bool,
    teamwork: bool,
    nodes: Vec<usize>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum GroupKey<'a> {
    Local(Option<&'a str>),
    Teamwork(&'a str),
}

fn rectangle(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
    Rectangle::new(Point::new(x, y), Size::new(width, height))
}

fn node(kind: NodeKind, title: String, subtitle: String) -> Node {
    Node {
        bounds: rectangle(0.0, 0.0, NODE_WIDTH, NODE_HEIGHT),
        kind,
        title,
        subtitle,
        badge: None,
    }
}

pub(super) fn build(window: &ConnectionWindow, width: f32) -> Layout {
    let Topology {
        nodes,
        members,
        edges,
        ..
    } = capture(window, true);
    place(nodes, &members, edges, width)
}

pub(crate) fn capture(window: &ConnectionWindow, tunnels: bool) -> Topology {
    let entries = window.visible_entries();
    let connection_count = entries.len();
    let mut computer = node(NodeKind::Computer, "This computer".into(), String::new());
    computer.bounds.x = 28.0;
    computer.bounds.y = ORIGIN_Y + ZONE_TITLE + ZONE_PADDING;
    let mut nodes = vec![computer];
    let mut zones = Vec::new();
    let mut by_group = HashMap::new();
    let mut by_host: HashMap<String, Vec<(u16, usize)>> = HashMap::new();
    let mut add_saved = |saved: Node, key| {
        let member = *by_group.entry(key).or_insert_with(|| {
            zones.push(match key {
                GroupKey::Local(group) => Members {
                    group: group.map(str::to_owned),
                    title: group
                        .map_or_else(|| "No folder".into(), |group| group.replace('/', " / ")),
                    color: group
                        .and_then(|group| window.store.inherited_group_color(group))
                        .map(|(_, color)| color.color()),
                    external: false,
                    teamwork: false,
                    nodes: Vec::new(),
                },
                GroupKey::Teamwork(group) => Members {
                    group: None,
                    title: teamwork::group_title(group),
                    color: None,
                    external: false,
                    teamwork: true,
                    nodes: Vec::new(),
                },
            });
            zones.len() - 1
        });
        let index = nodes.len();
        zones[member].nodes.push(index);
        nodes.push(saved);
        index
    };
    let mut routes = Vec::with_capacity(connection_count);
    for row in entries {
        let (connection, key, group, badge) = match row {
            VisualRow::Connection(connection, _) => (
                connection,
                RowKey::Connection(connection.id),
                GroupKey::Local(window.store.group_of(connection.id)),
                window.connection_badge(connection.id),
            ),
            VisualRow::Shared(entry) => (
                &entry.connection,
                RowKey::Shared(entry.identity.clone()),
                GroupKey::Teamwork(&entry.group),
                os_badge::Badge::resolve(entry.badge.as_deref(), &window.custom_badges),
            ),
            VisualRow::Serial(serial, _) => {
                add_saved(
                    node(
                        NodeKind::Saved(RowKey::Serial(serial.id)),
                        serial.display_name(),
                        format!("{} · {}", serial.connection.device.display(), serial.line()),
                    ),
                    GroupKey::Local(window.store.group_of(serial.id)),
                );
                continue;
            }
            VisualRow::Local(profile) => {
                add_saved(
                    node(
                        NodeKind::Saved(RowKey::Local(profile.id)),
                        profile.display_name(),
                        "Local shell".into(),
                    ),
                    GroupKey::Local(None),
                );
                continue;
            }
            VisualRow::Group { .. } => unreachable!("map entries contain connections only"),
        };
        let endpoint = &connection.login.endpoint;
        let host = address(&endpoint.host, endpoint.port);
        let subtitle = if endpoint.username.is_empty() {
            host
        } else {
            format!("{}@{host}", endpoint.username)
        };
        let mut saved = node(NodeKind::Saved(key), connection.name.clone(), subtitle);
        saved.badge = badge.and_then(super::pdf::NodeBadge::capture);
        let index = add_saved(saved, group);
        by_host
            .entry(endpoint.host.to_ascii_lowercase())
            .or_default()
            .push((endpoint.port, index));
        routes.push((connection, index));
    }
    let mut external = HashMap::new();
    let mut external_nodes = Vec::new();
    let mut edges = Vec::new();
    let mut jump_routes = std::collections::HashSet::new();
    for (connection, target) in routes {
        if let Some(jump) = &connection.jump {
            let jump = &jump.endpoint;
            let host = jump.host.to_ascii_lowercase();
            let source = by_host
                .get(&host)
                .and_then(|candidates| {
                    candidates
                        .iter()
                        .filter(|&&(_, node)| node != target)
                        .min_by_key(|&&(port, _)| port != jump.port)
                        .map(|&(_, node)| node)
                })
                .unwrap_or_else(|| {
                    *external.entry((host, jump.port)).or_insert_with(|| {
                        let index = nodes.len();
                        nodes.push(node(
                            NodeKind::Jump,
                            address(&jump.host, jump.port),
                            String::new(),
                        ));
                        external_nodes.push(index);
                        index
                    })
                });
            if jump_routes.insert(source) {
                edges.push(Edge {
                    from: COMPUTER,
                    to: source,
                    kind: EdgeKind::Jump,
                    label: String::new(),
                });
            }
            edges.push(Edge {
                from: source,
                to: target,
                kind: EdgeKind::Jump,
                label: String::new(),
            });
        }
        if tunnels && let Some(label) = tunnel_summary(connection) {
            edges.push(Edge {
                from: COMPUTER,
                to: target,
                kind: EdgeKind::Tunnel,
                label,
            });
        }
    }
    if !external_nodes.is_empty() {
        zones.push(Members {
            group: None,
            title: "External jump servers".into(),
            color: None,
            external: true,
            teamwork: false,
            nodes: external_nodes,
        });
    }
    let mut links = vec![0_usize; nodes.len()];
    for edge in &edges {
        links[edge.from] += 1;
        links[edge.to] += 1;
    }
    zones.sort_by_cached_key(|zone| {
        (
            Reverse(zone.nodes.iter().map(|&id| links[id]).sum::<usize>()),
            zone.external,
            zone.teamwork,
            zone.group.as_deref().map(str::to_lowercase),
        )
    });
    Topology {
        scope: scope_title(window),
        connection_count,
        nodes,
        members: zones,
        edges,
    }
}

impl Topology {
    pub(super) fn place(&self, width: f32) -> Layout {
        place(self.nodes.clone(), &self.members, self.edges.clone(), width)
    }
}

fn scope_title(window: &ConnectionWindow) -> String {
    match &window.scope {
        Scope::All => "All connections".into(),
        Scope::Favorites => "Favorites".into(),
        Scope::Recent => "Recently used".into(),
        Scope::Open => "Open now".into(),
        Scope::Teamwork => "Teamwork connections".into(),
        Scope::Group(path) => path.replace('/', " / "),
        Scope::Tag(tag) => {
            let display = window
                .store
                .connections()
                .iter()
                .chain(window.shared_entries().map(|entry| &entry.connection))
                .find(|entry| entry.tag.to_lowercase() == *tag)
                .map_or(tag.as_str(), |entry| &entry.tag);
            format!("Tag: {display}")
        }
    }
}

fn place(mut nodes: Vec<Node>, members: &[Members], edges: Vec<Edge>, width: f32) -> Layout {
    let limit = width.max(ORIGIN_X + zone_width(1) + 20.0);
    let (mut x, mut y) = (ORIGIN_X, ORIGIN_Y);
    let mut shelf_height = 0.0_f32;
    let mut map_width = ORIGIN_X + NODE_WIDTH;
    let mut zones = Vec::with_capacity(members.len());
    for zone in members {
        let columns = columns(zone.nodes.len());
        let rows = zone.nodes.len().div_ceil(columns) as f32;
        let width = zone_width(columns);
        let height =
            ZONE_TITLE + ZONE_PADDING * 2.0 + rows * NODE_HEIGHT + (rows - 1.0) * NODE_GAP_Y;
        if x > ORIGIN_X && x + width > limit {
            x = ORIGIN_X;
            y += shelf_height + ZONE_GAP;
            shelf_height = 0.0;
        }
        for (index, &node) in zone.nodes.iter().enumerate() {
            nodes[node].bounds.x =
                x + ZONE_PADDING + (index % columns) as f32 * (NODE_WIDTH + NODE_GAP_X);
            nodes[node].bounds.y = y
                + ZONE_TITLE
                + ZONE_PADDING
                + (index / columns) as f32 * (NODE_HEIGHT + NODE_GAP_Y);
        }
        zones.push(Zone {
            bounds: rectangle(x, y, width, height),
            group: zone.group.clone(),
            title: zone.title.clone(),
            color: zone.color,
            drop_target: !zone.external && !zone.teamwork,
            teamwork: zone.teamwork,
        });
        x += width + ZONE_GAP;
        map_width = map_width.max(x);
        shelf_height = shelf_height.max(height);
    }
    Layout {
        zones,
        nodes,
        edges,
        size: Size::new(map_width, (y + shelf_height + ORIGIN_Y).max(200.0)),
    }
}

fn columns(count: usize) -> usize {
    match count {
        0..=1 => 1,
        2..=4 => 2,
        _ => 3,
    }
}

fn zone_width(columns: usize) -> f32 {
    ZONE_PADDING * 2.0 + columns as f32 * NODE_WIDTH + (columns - 1) as f32 * NODE_GAP_X
}

fn address(host: &str, port: u16) -> String {
    if port == 22 {
        host.into()
    } else {
        host_port(host, port)
    }
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn tunnel_summary(connection: &Connection) -> Option<String> {
    let mut enabled = connection.tunnels.iter().filter(|tunnel| tunnel.enabled);
    let rule = &enabled.next()?.rule;
    let listen = rule.bind.port();
    let mut summary = match &rule.target {
        TunnelTarget::Local { host, port } => format!("L {listen} → {}", host_port(host, *port)),
        TunnelTarget::Remote { host, port } => format!("R {listen} → {}", host_port(host, *port)),
        TunnelTarget::Dynamic => format!("D {listen} (SOCKS)"),
    };
    let remaining = enabled.count();
    if remaining != 0 {
        summary.push_str(&format!(" +{remaining}"));
    }
    Some(summary)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::connection_store::{Auth, SavedLogin, Store, TabColor, TunnelConfig, TunnelRule};
    use crate::ssh_dialog::LoginTarget;

    use super::*;

    struct TestProfile(PathBuf);

    impl TestProfile {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "kortty-connection-map-{:016x}",
                u64::from_ne_bytes(nonce)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn login(host: &str, port: u16) -> SavedLogin {
        SavedLogin {
            endpoint: LoginTarget {
                host: host.into(),
                port,
                username: "operator".into(),
            },
            auth: Auth::Prompt,
            keyboard_interactive: false,
        }
    }

    fn connection(name: &str, host: &str, port: u16, jump: Option<SavedLogin>) -> Connection {
        Connection {
            id: 0,
            name: name.into(),
            tag: String::new(),
            login: login(host, port),
            jump,
            emulation: "xterm-256color".into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        }
    }

    #[test]
    fn connection_map_matches_visible_routes_and_shelves_zones() {
        let profile = TestProfile::new();
        let path = profile.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let production = store.create_group(None, "Production").unwrap();
        let north = store.create_group(Some(&production), "North").unwrap();
        let color = TabColor::parse("#226688").unwrap();
        store.set_group_color(&production, Some(color)).unwrap();
        let fallback = store
            .save(connection("A fallback", "bastion.example", 22, None))
            .unwrap();
        let exact = store
            .save(connection("B exact", "bastion.example", 2222, None))
            .unwrap();
        let mut database = connection(
            "Database",
            "db.example",
            22,
            Some(login("bastion.example", 2222)),
        );
        for (listen, enabled) in [(15432, true), (15433, false), (15434, true)] {
            database.tunnels.push(TunnelConfig {
                enabled,
                description: String::new(),
                rule: TunnelRule {
                    bind: ([127, 0, 0, 1], listen).into(),
                    target: TunnelTarget::Local {
                        host: "localhost".into(),
                        port: 5432,
                    },
                },
            });
        }
        let database = store.save_in_group(database, Some(&north)).unwrap();
        let worker = store
            .save_in_group(
                connection(
                    "Worker",
                    "worker.example",
                    22,
                    Some(login("bastion.example", 2299)),
                ),
                Some(&production),
            )
            .unwrap();
        for name in ["External one", "External two"] {
            store
                .save(connection(
                    name,
                    "target.example",
                    22,
                    Some(login("outside.example", 2200)),
                ))
                .unwrap();
        }
        let mut window = ConnectionWindow::open(path).unwrap();
        let saved = capture(&window, true);
        let no_tunnels = capture(&window, false);
        let narrow = saved.place(480.0);
        let wide = saved.place(2400.0);
        assert!(
            narrow
                .zones
                .windows(2)
                .all(|zones| zones[0].bounds.y < zones[1].bounds.y)
        );
        assert!(wide.zones.iter().all(|zone| zone.bounds.y == ORIGIN_Y));
        assert_eq!(narrow.nodes[COMPUTER].kind, NodeKind::Computer);
        let index = |id| {
            narrow
                .nodes
                .iter()
                .position(|node| node.kind == NodeKind::Saved(RowKey::Connection(id)))
                .unwrap()
        };
        let route = |from, to| {
            narrow.edges.iter().any(|edge| {
                edge.kind == EdgeKind::Jump && edge.from == index(from) && edge.to == index(to)
            })
        };
        assert!(route(exact, database));
        assert!(route(fallback, worker));
        let external = narrow
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::Jump)
            .unwrap();
        assert_eq!(
            narrow
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Jump)
                .count(),
            1
        );
        assert_eq!(
            narrow
                .edges
                .iter()
                .filter(|edge| edge.from == COMPUTER && edge.to == external)
                .count(),
            1
        );
        assert_eq!(
            narrow
                .edges
                .iter()
                .filter(|edge| edge.from == external)
                .count(),
            2
        );
        let tunnel = narrow
            .edges
            .iter()
            .find(|edge| edge.kind == EdgeKind::Tunnel)
            .unwrap();
        assert_eq!(
            (tunnel.from, tunnel.to, tunnel.label.as_str()),
            (COMPUTER, index(database), "L 15432 → localhost:5432 +1")
        );
        assert_eq!(
            narrow
                .zones
                .iter()
                .find(|zone| zone.group.as_deref() == Some(north.as_str()))
                .unwrap()
                .color,
            Some(color.color())
        );
        assert_eq!(saved.scope, "All connections");
        assert_eq!(saved.connection_count, 6);
        let quiet = no_tunnels.place(480.0);
        let group = |layout: &Layout, path: &str| {
            layout
                .zones
                .iter()
                .position(|zone| zone.group.as_deref() == Some(path))
                .unwrap()
        };
        assert!(group(&narrow, &north) < group(&narrow, &production));
        assert!(group(&quiet, &production) < group(&quiet, &north));
        assert!(quiet.edges.iter().all(|edge| edge.kind == EdgeKind::Jump));
        assert!(
            quiet
                .edges
                .iter()
                .map(|edge| (edge.from, edge.to))
                .eq(narrow
                    .edges
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::Jump)
                    .map(|edge| (edge.from, edge.to)))
        );

        let _ = window.update(super::super::super::Message::Search("Database".into()));
        let filtered = build(&window, 480.0);
        assert_eq!(filtered.nodes.len(), 3);
        assert!(
            filtered
                .nodes
                .iter()
                .any(|node| node.kind == NodeKind::Jump && node.title == "bastion.example:2222")
        );
        drop(window);
        assert_eq!(saved.place(1600.0).nodes.len(), narrow.nodes.len());
    }

    #[test]
    fn teamwork_map_separates_shared_groups_and_keeps_last_good_sources() {
        use super::super::super::Message;

        let profile = TestProfile::new();
        let path = profile.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let folder = store.create_group(None, "Production").unwrap();
        let color = TabColor::parse("#226688").unwrap();
        store.set_group_color(&folder, Some(color)).unwrap();
        let gateway = store
            .save_in_group(
                connection("Local gateway", "gateway.example", 22, None),
                Some(&folder),
            )
            .unwrap();
        let mut settings = teamwork::Settings::default();
        for (file, name) in [
            ("first.xml", "Shared primary"),
            ("second.xml", "Shared spare"),
        ] {
            let source = profile.0.join(file);
            fs::write(
                &source,
                format!(
                    r#"<connections><connection id="same-external-id">
                        <name>{name}</name><host>server.example</host><username>operator</username>
                        <group>Production</group><jumpServer><enabled>true</enabled>
                        <host>gateway.example</host><username>operator</username>
                        </jumpServer></connection></connections>"#
                ),
            )
            .unwrap();
            settings.sources.push(teamwork::Source {
                location: source.into_os_string().into_string().unwrap(),
                enabled: true,
                kind: teamwork::SourceKind::File,
                check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
            });
        }
        let catalog =
            teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
        let mut window = ConnectionWindow::open(path).unwrap();
        window.set_teamwork(catalog.clone(), None);
        let snapshot = capture(&window, true);
        assert_eq!(snapshot.connection_count, 3);
        let layout = snapshot.place(2000.0);
        let shared = layout.zones.iter().find(|zone| zone.teamwork).unwrap();
        assert_eq!(shared.title, "Teamwork / Production");
        assert!(!shared.drop_target);
        assert!(shared.color.is_none() && shared.group.is_none());
        let local = layout.zones.iter().find(|zone| !zone.teamwork).unwrap();
        assert!(local.drop_target && local.color == Some(color.color()));
        assert_eq!(local.group.as_deref(), Some(folder.as_str()));
        let local = layout
            .nodes
            .iter()
            .position(|node| node.kind == NodeKind::Saved(RowKey::Connection(gateway)))
            .unwrap();
        let shared: Vec<_> = layout
            .nodes
            .iter()
            .filter(|node| node.is_shared())
            .collect();
        assert_eq!(shared.len(), 2);
        assert_ne!(shared[0].kind, shared[1].kind);
        assert_eq!(
            layout
                .edges
                .iter()
                .filter(|edge| edge.from == local)
                .count(),
            2
        );

        let NodeKind::Saved(shared_row) = &shared[0].kind else {
            unreachable!()
        };
        window.select_map_row(shared_row.clone());
        assert!(window.selected().is_none() && window.selected_shared().is_some());
        let _ = window.update(Message::Scope(Scope::Group(folder)));
        assert_eq!(capture(&window, true).connection_count, 1);
        let _ = window.update(Message::Scope(Scope::Teamwork));
        assert_eq!(capture(&window, true).connection_count, 2);

        fs::write(&settings.sources[0].location, "invalid").unwrap();
        settings.sources[1].enabled = false;
        let retained = catalog.refreshed(&settings.active_sources(true, &[]).unwrap());
        assert_eq!(retained.failures, 1);
        window.set_teamwork(retained, None);
        let remaining = capture(&window, true).place(600.0);
        assert!(
            remaining
                .nodes
                .iter()
                .any(|node| node.title == "Shared primary")
        );
        assert!(
            remaining
                .nodes
                .iter()
                .all(|node| node.title != "Shared spare")
        );
    }
}
