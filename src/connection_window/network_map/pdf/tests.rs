use std::collections::HashSet;
use std::io::Read as _;
use std::path::PathBuf;

use flate2::read::ZlibDecoder;
use iced::{Point, Size};

use crate::connection_store::custom_os_badges::Catalog;
use crate::connection_store::{Auth, Connection, SavedLogin, Store};
use crate::connection_window::{ConnectionWindow, RowKey};
use crate::ssh_dialog::LoginTarget;

use super::super::layout::{NodeKind, capture};
use super::*;

#[test]
fn one_tall_map_zone_keeps_every_node_whole_across_page_cuts() {
    let nodes = (0..100)
        .map(|index| Node {
            bounds: Rectangle::new(
                Point::new(284.0, 66.0 + 66.0 * index as f32),
                Size::new(176.0, 44.0),
            ),
            kind: NodeKind::Saved(RowKey::Connection(index)),
            title: index.to_string(),
            subtitle: String::new(),
            badge: None,
        })
        .collect::<Vec<_>>();
    let height = 100.0 * 66.0 + 75.0;
    let layout = Layout {
        zones: vec![Zone {
            bounds: Rectangle::new(Point::new(270.0, 24.0), Size::new(204.0, height - 48.0)),
            group: None,
            title: "No folder".into(),
            color: None,
            drop_target: true,
            teamwork: false,
        }],
        nodes,
        edges: Vec::new(),
        size: Size::new(508.0, height),
    };
    let pages = bands(&layout, 260.0).unwrap();
    assert!(pages.len() > 1);
    assert!(pages[0].bottom >= 66.0 + 44.0);
    assert_eq!(pages.first().unwrap().top, 0.0);
    assert_eq!(pages.last().unwrap().bottom, height);
    assert!(pages.iter().all(|band| band.bottom - band.top <= 260.0));
    for node in &layout.nodes {
        assert_eq!(
            pages
                .iter()
                .filter(|band| {
                    band.top <= node.bounds.y && node.bounds.y + node.bounds.height <= band.bottom
                })
                .count(),
            1
        );
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kortty-map-pdf-{}",
            crate::private_files::nonce().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn connection_map_pdf_writes_a4_metadata_searchable_text_and_vector_routes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    let login = |host: &str| SavedLogin {
        endpoint: LoginTarget {
            host: host.into(),
            port: 22,
            username: "operator".into(),
        },
        auth: Auth::Prompt,
        keyboard_interactive: false,
    };
    let connection = |name: &str, host: &str| Connection {
        id: 0,
        name: name.into(),
        tag: String::new(),
        login: login(host),
        jump: Some(login("jump.example")),
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
    };
    let symbol_path = fixture.0.join("source.png");
    image::RgbaImage::from_pixel(48, 32, image::Rgba([24, 112, 205, 255]))
        .save(&symbol_path)
        .unwrap();
    let mut catalog = Catalog::load(&fixture.0).unwrap();
    let custom = catalog.add("Router", "Network", &symbol_path).unwrap();
    for (name, host, badge) in [
        ("Ж Endpoint", "target.example", "windows-11"),
        ("Backup", "backup.example", "windows-11"),
        ("Router", "router.example", custom.as_str()),
        (
            "Missing symbol",
            "missing.example",
            "custom:00000000-0000-0000-0000-000000000001",
        ),
    ] {
        store
            .save_in_group_with_badge(connection(name, host), None, Some(badge))
            .unwrap();
    }
    let manager = ConnectionWindow::open(path.clone()).unwrap();
    let snapshot = capture(&manager, false);
    catalog.remove(&custom).unwrap();
    for landscape in [false, true] {
        let pdf = render(&snapshot, "Fixture map", landscape, &PdfBranding::default()).unwrap();
        let dictionary = String::from_utf8_lossy(&pdf);
        assert!(dictionary.contains("/Title (Fixture map)"));
        assert!(dictionary.contains("/CreationDate (D:"));
        assert!(dictionary.contains("/ToUnicode "));
        let media_box = dictionary
            .split_once("/MediaBox [")
            .unwrap()
            .1
            .split_once(']')
            .unwrap()
            .0;
        let dimensions = media_box
            .split_ascii_whitespace()
            .map(|value| value.parse::<f32>().unwrap())
            .collect::<Vec<_>>();
        let (width, height) = (dimensions[2], dimensions[3]);
        assert_eq!(width > height, landscape);
        assert!((width.min(height) - 595.2756).abs() < 0.01);
        assert!((width.max(height) - 841.8898).abs() < 0.01);

        let streams = decoded_streams(&pdf);
        assert!(
            streams
                .iter()
                .any(|stream| String::from_utf8_lossy(stream).contains("<0416>"))
        );
        let vectors = streams
            .iter()
            .find_map(|stream| {
                let text = std::str::from_utf8(stream).ok()?;
                text.split_once("W\nn\n").map(|(_, clipped)| clipped)
            })
            .expect("clipped map page stream");
        assert!(vectors.contains(" d\n"));
        assert!(vectors.contains(" c\nS\n"));
        assert!(vectors.contains("BT\n"));

        let (page_width, page_height) = (width - 2.0 * MARGIN, height - HEADER - BOTTOM);
        let side = 22.0 * Plan::new(&snapshot, page_width, page_height).unwrap().scale;
        let draws = vectors
            .lines()
            .zip(vectors.lines().skip(1))
            .filter(|(_, line)| line.starts_with("/Im") && line.ends_with(" Do"))
            .collect::<Vec<_>>();
        assert_eq!(draws.len(), 3);
        assert_eq!(
            draws
                .iter()
                .map(|&(_, image)| image)
                .collect::<HashSet<_>>()
                .len(),
            2
        );
        for (matrix, _) in draws {
            let values = matrix
                .strip_suffix(" cm")
                .unwrap()
                .split_whitespace()
                .map(|value| value.parse::<f32>().unwrap())
                .collect::<Vec<_>>();
            assert!((values[0] - side).abs() < 0.01);
            assert!((values[3] - side).abs() < 0.01);
        }
    }
    let reopened = ConnectionWindow::open(path).unwrap();
    let reopened = capture(&reopened, false);
    let reopened = render(&reopened, "Reopened map", true, &PdfBranding::default()).unwrap();
    let draws = decoded_streams(&reopened)
        .iter()
        .filter_map(|stream| std::str::from_utf8(stream).ok())
        .flat_map(|text| text.lines())
        .filter(|line| line.starts_with("/Im") && line.ends_with(" Do"))
        .count();
    assert_eq!(
        draws, 3,
        "logo and two connections with the remaining built-in symbol"
    );
}

#[test]
fn every_builtin_os_badge_has_a_nonempty_native_raster() {
    for badge in OsBadge::all() {
        let image = badge
            .raster()
            .unwrap_or_else(|error| panic!("{}: {error}", badge.id()));
        assert_eq!(image.dimensions(), (128, 128));
        assert!(
            image.pixels().any(|pixel| pixel.0[3] == 0),
            "{}",
            badge.id()
        );
        assert!(
            image.pixels().any(|pixel| pixel.0[3] == 255),
            "{}",
            badge.id()
        );
    }
}

fn decoded_streams(mut pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut decoded = Vec::new();
    const STREAM: &[u8] = b"\nstream\n";
    while let Some(start) = pdf.windows(STREAM.len()).position(|bytes| bytes == STREAM) {
        let dictionary = pdf[..start]
            .windows(2)
            .rposition(|bytes| bytes == b"<<")
            .unwrap();
        let header = std::str::from_utf8(&pdf[dictionary..start]).unwrap();
        let length = header
            .split_once("/Length ")
            .unwrap()
            .1
            .split_ascii_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let content = &pdf[start + STREAM.len()..];
        let mut stream = Vec::new();
        ZlibDecoder::new(&content[..length])
            .read_to_end(&mut stream)
            .unwrap();
        decoded.push(stream);
        pdf = &content[length..];
    }
    decoded
}
