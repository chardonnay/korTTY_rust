//! Searchable vector PDF from the Connection Manager's scoped map.

use std::collections::{HashMap, hash_map::Entry};
use std::io;

use iced::{Color, Rectangle};
use jiff::{Timestamp, tz::TimeZone};

use crate::connection_store::OsBadge;
use crate::connection_store::custom_os_badges::CustomBadge;
use crate::session_journal::export::pdf::{
    Font, Layout as Pdf, MARGIN, Metadata, Orientation, PageBlock, Style, VectorStroke,
};
use crate::session_journal::export_branding::{PdfBranding, brand_input};

use super::super::os_badge::Badge;
use super::layout::{EdgeKind, Layout, Node, Topology, Zone};

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_TITLE: &str = "Connection map";

const LOGO: &[u8] = include_bytes!("../../workspace/chrome/about/kortty_logo.png");
const HEADER: f32 = 98.0;
const BOTTOM: f32 = 62.0;
const MIN_SCALE: f32 = 0.45;
const TEXT: [u8; 3] = [0x1f, 0x29, 0x37];
const MUTED: [u8; 3] = [0x60, 0x6b, 0x7a];
const RULE: [u8; 3] = [0xe5, 0xe7, 0xeb];
const JUMP: [u8; 3] = [0xdc, 0x98, 0x25];
const TUNNEL: [u8; 3] = [0x13, 0xa8, 0xad];
const TUNNEL_TEXT: [u8; 3] = [0x0e, 0x76, 0x7a];
const BORDER: [u8; 3] = [0x8a, 0x94, 0xa3];

pub(crate) fn render(
    snapshot: &Topology,
    title: &str,
    landscape: bool,
    branding: &PdfBranding,
) -> io::Result<Vec<u8>> {
    let orientation = if landscape {
        Orientation::Landscape
    } else {
        Orientation::Portrait
    };
    let [width, height] = orientation.size();
    let content_width = width - 2.0 * MARGIN;
    let content_height = height - HEADER - BOTTOM;
    let plan = Plan::new(snapshot, content_width, content_height)?;
    let title = brand_input(title);
    let title = if title.trim().is_empty() {
        DEFAULT_TITLE
    } else {
        title.trim()
    };
    let now = Timestamp::now();
    let local_time = now.to_zoned(TimeZone::system());
    let date = local_time.strftime("%Y-%m-%d %H:%M");
    let count = snapshot.connection_count;
    let noun = if count == 1 {
        "connection"
    } else {
        "connections"
    };
    let subtitle = format!("{} · {count} {noun} · {date}", snapshot.scope);

    let mut pdf = Pdf::with_orientation(&|| false, orientation)?;
    let logo = Logo::embed(&mut pdf)?;
    let badges = embed_badges(&mut pdf, &plan.layout.nodes)?;
    let teamwork = plan.layout.nodes.iter().any(Node::is_shared);
    let tunnels = TunnelList::new(&pdf, &plan, height - BOTTOM);
    let pages = plan.bands.len().max(tunnels.pages.len());
    let left = (content_width - plan.layout.size.width * plan.scale) / 2.0;
    for index in 0..pages {
        if index > 0 {
            pdf.start_page(orientation)?;
        }
        let mut page = pdf.page_block();
        header(&mut page, content_width, title, &subtitle, logo, teamwork)?;
        if let Some(&band) = plan.bands.get(index) {
            let frame = MapFrame {
                left,
                scale: plan.scale,
                band,
            };
            let band_height = ((band.bottom - band.top) * plan.scale).min(content_height);
            page.clipped([0.0, HEADER, content_width, band_height], |page| {
                frame.draw(page, &plan.layout, &badges)
            })?;
        }
        if let Some(rows) = tunnels.pages.get(index) {
            for (baseline, text) in rows {
                page.label(
                    [left + tunnels.left, *baseline],
                    tunnels.width,
                    tunnels.style,
                    text,
                )?;
            }
        }
    }
    pdf.finish_with_metadata(
        branding,
        Some(Metadata {
            title,
            subject: &subtitle,
            keywords: "kortty, connection map",
            created: now,
        }),
    )
}

/// A badge resolved from the visible connection and the current profile catalog.
#[derive(Clone)]
pub(super) enum NodeBadge {
    BuiltIn(OsBadge),
    Custom(CustomBadge),
}

impl NodeBadge {
    pub(super) fn capture(badge: Badge<'_>) -> Option<Self> {
        match badge {
            Badge::BuiltIn(badge) => Some(Self::BuiltIn(badge)),
            Badge::Custom(badge) if !badge.png().is_empty() => Some(Self::Custom(badge.clone())),
            Badge::Custom(_) => None,
        }
    }

    fn reference(&self) -> &str {
        match self {
            Self::BuiltIn(badge) => badge.id(),
            Self::Custom(badge) => badge.reference(),
        }
    }

    fn raster(&self) -> io::Result<image::RgbaImage> {
        match self {
            Self::BuiltIn(badge) => badge.raster(),
            Self::Custom(badge) => badge.raster(),
        }
    }
}

#[derive(Clone, Copy)]
struct BadgeImage {
    index: usize,
    aspect: f32,
}

type Badges<'a> = HashMap<&'a str, Option<BadgeImage>>;

fn embed_badges<'a>(pdf: &mut Pdf<'_>, nodes: &'a [Node]) -> io::Result<Badges<'a>> {
    let mut badges = HashMap::new();
    for badge in nodes.iter().filter_map(|node| node.badge.as_ref()) {
        let Entry::Vacant(slot) = badges.entry(badge.reference()) else {
            continue;
        };
        let image = match badge.raster() {
            Ok(image) => Some(BadgeImage {
                index: pdf.embed_rgba(&image)?,
                aspect: image.width() as f32 / image.height() as f32,
            }),
            Err(_) => None,
        };
        slot.insert(image);
    }
    Ok(badges)
}

struct Plan {
    layout: Layout,
    scale: f32,
    bands: Vec<Band>,
}

impl Plan {
    fn new(snapshot: &Topology, width: f32, height: f32) -> io::Result<Self> {
        let mut best = (0.0, 600.0);
        let mut shelved = (f32::INFINITY, 600.0);
        for candidate in (600..=2600).step_by(100) {
            let candidate = candidate as f32;
            let size = snapshot.place(candidate).size;
            let fit = (width / size.width).min(height / size.height).min(1.0);
            if fit > best.0 {
                best = (fit, candidate);
            }
            if size.width * MIN_SCALE <= width && size.height < shelved.0 {
                shelved = (size.height, candidate);
            }
        }
        let (layout, scale) = if best.0 >= MIN_SCALE {
            (snapshot.place(best.1), best.0)
        } else {
            let layout = snapshot.place(shelved.1);
            let scale = MIN_SCALE.min(width / layout.size.width);
            (layout, scale)
        };
        let bands = bands(&layout, height / scale)?;
        Ok(Self {
            layout,
            scale,
            bands,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Band {
    top: f32,
    bottom: f32,
}

fn bands(layout: &Layout, page_height: f32) -> io::Result<Vec<Band>> {
    let shelves = gaps(layout.zones.iter().map(|zone| zone.bounds));
    let rows = gaps(layout.nodes.iter().map(|node| node.bounds));
    let mut pages = Vec::new();
    let mut top = 0.0;
    while top < layout.size.height {
        let limit = top + page_height;
        let bottom = if layout.size.height <= limit {
            layout.size.height
        } else {
            // Prefer a shelf boundary. A shelf taller than one page needs a cut
            // between complete rows so all of its nodes remain visible.
            cut(&shelves, top, limit)
                .or_else(|| cut(&rows, top, limit))
                .ok_or_else(|| {
                    io::Error::other("The connection map cannot fit its nodes on an A4 page.")
                })?
        };
        pages.push(Band { top, bottom });
        top = bottom;
    }
    Ok(pages)
}

fn gaps(rectangles: impl Iterator<Item = Rectangle>) -> Vec<f32> {
    let mut ranges = rectangles
        .map(|rect| (rect.y, rect.y + rect.height))
        .collect::<Vec<_>>();
    ranges.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut gaps = Vec::new();
    let Some(&(_, mut bottom)) = ranges.first() else {
        return gaps;
    };
    for (top, end) in ranges.into_iter().skip(1) {
        if top > bottom {
            gaps.push(f32::midpoint(top, bottom));
        }
        bottom = bottom.max(end);
    }
    gaps
}

fn cut(gaps: &[f32], top: f32, limit: f32) -> Option<f32> {
    let index = gaps.partition_point(|&gap| gap <= limit);
    index
        .checked_sub(1)
        .map(|i| gaps[i])
        .filter(|&gap| gap > top)
}

#[derive(Clone, Copy)]
struct Logo {
    image: usize,
    width: f32,
}

impl Logo {
    fn embed(pdf: &mut Pdf<'_>) -> io::Result<Self> {
        let image = image::load_from_memory_with_format(LOGO, image::ImageFormat::Png)
            .map_err(io::Error::other)?;
        let width = 28.0 * image.width() as f32 / image.height() as f32;
        let image = image
            .resize(u32::MAX, 120, image::imageops::FilterType::Triangle)
            .into_rgba8();
        Ok(Self {
            image: pdf.embed_rgba(&image)?,
            width,
        })
    }
}

fn header(
    page: &mut PageBlock<'_>,
    width: f32,
    title: &str,
    subtitle: &str,
    logo: Logo,
    teamwork: bool,
) -> io::Result<()> {
    page.image(logo.image, [0.0, 28.0, logo.width, 28.0])?;
    let left = logo.width + 12.0;
    page.label(
        [left, 40.0],
        width - left,
        Style::new(Font::Bold, 17.0, TEXT),
        title,
    )?;
    page.label(
        [left, 55.0],
        width - left,
        Style::new(Font::Regular, 9.0, MUTED),
        subtitle,
    )?;
    page.rect([0.0, 66.0, width, 0.7], RULE)?;
    let legend = Style::new(Font::Regular, 8.5, TEXT);
    for (kind, x, label) in [
        (EdgeKind::Jump, 0.0, "Jump routes"),
        (EdgeKind::Tunnel, 110.0, "Tunnels"),
    ] {
        page.curve(
            [[x, 79.0], [x, 79.0], [x + 22.0, 79.0], [x + 22.0, 79.0]],
            edge_stroke(kind, 1.0),
        )?;
        page.label([x + 28.0, 82.0], 75.0, legend, label)?;
    }
    if teamwork {
        page.round_rect([220.0, 74.0, 18.0, 10.0], 2.0, [255; 3], shared_stroke(0.9))?;
        page.label([244.0, 82.0], 90.0, legend, "Teamwork")?;
    }
    Ok(())
}

fn shared_stroke(width: f32) -> VectorStroke {
    VectorStroke {
        color: BORDER,
        width,
        dash: Some([3.0, 2.0]),
        round_caps: false,
    }
}

struct MapFrame {
    left: f32,
    scale: f32,
    band: Band,
}

impl MapFrame {
    fn point(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        [
            self.left + x * self.scale,
            HEADER + (y - self.band.top) * self.scale,
        ]
    }

    fn rect(&self, bounds: Rectangle) -> [f32; 4] {
        let [left, top] = self.point([bounds.x, bounds.y]);
        [
            left,
            top,
            bounds.width * self.scale,
            bounds.height * self.scale,
        ]
    }

    fn visible(&self, top: f32, bottom: f32) -> bool {
        top < self.band.bottom && bottom > self.band.top
    }

    fn draw(
        &self,
        page: &mut PageBlock<'_>,
        layout: &Layout,
        badges: &Badges<'_>,
    ) -> io::Result<()> {
        for zone in &layout.zones {
            if self.visible(zone.bounds.y, zone.bounds.y + zone.bounds.height) {
                self.zone(page, zone)?;
            }
        }
        for edge in &layout.edges {
            let from = layout.nodes[edge.from].bounds;
            let to = layout.nodes[edge.to].bounds;
            let y0 = from.center_y();
            let y1 = to.center_y();
            if self.visible(y0.min(y1), y0.max(y1)) {
                page.curve(
                    edge_points(from, to).map(|point| self.point(point)),
                    edge_stroke(edge.kind, self.scale),
                )?;
            }
        }
        for node in &layout.nodes {
            if self.visible(node.bounds.y, node.bounds.y + node.bounds.height) {
                let badge = node
                    .badge
                    .as_ref()
                    .and_then(|badge| badges.get(badge.reference()).copied().flatten());
                self.node(page, node, badge)?;
            }
        }
        Ok(())
    }

    fn zone(&self, page: &mut PageBlock<'_>, zone: &Zone) -> io::Result<()> {
        let color = zone.color.map_or([0x80; 3], color_bytes);
        let fill = color.map(|part| (255.0 - (255.0 - f32::from(part)) * 0.08).round() as u8);
        let outline = zone.color.map_or([0x9a, 0xa0, 0xa6], color_bytes);
        let bounds = self.rect(zone.bounds);
        let [left, top, width, _] = bounds;
        page.round_rect(
            bounds,
            (14.0 * self.scale).max(3.0),
            fill,
            VectorStroke {
                color: outline,
                width: 0.9 + 0.4 * growth(self.scale),
                dash: Some([(7.0 * self.scale).max(3.0), (5.0 * self.scale).max(2.5)]),
                round_caps: false,
            },
        )?;
        if zone.bounds.y >= self.band.top {
            let size = 7.5 + 2.5 * growth(self.scale);
            let pad = (14.0 * self.scale).max(4.0);
            let square = size * 0.8;
            let center = top + 14.0 * self.scale;
            page.rect([left + pad, center - square / 2.0, square, square], outline)?;
            page.label(
                [left + pad + square + 5.0, center + size * 0.35],
                width - 2.0 * pad - square - 5.0,
                Style::new(Font::Bold, size, TEXT),
                &zone.title,
            )?;
        }
        Ok(())
    }

    fn node(
        &self,
        page: &mut PageBlock<'_>,
        node: &Node,
        badge: Option<BadgeImage>,
    ) -> io::Result<()> {
        let bounds = self.rect(node.bounds);
        let [left, top, width, height] = bounds;
        let growth = growth(self.scale);
        let border_width = 0.8 + 0.2 * growth;
        page.round_rect(
            bounds,
            (9.0 * self.scale).max(2.5),
            [255; 3],
            if node.is_shared() {
                shared_stroke(border_width)
            } else {
                VectorStroke {
                    color: BORDER,
                    width: border_width,
                    dash: None,
                    round_caps: false,
                }
            },
        )?;
        let name = Style::new(Font::Bold, 7.5 + 3.0 * growth, TEXT);
        let address = Style::new(Font::Regular, 6.5 + 2.5 * growth, MUTED);
        let pad = (9.0 * self.scale).max(4.0);
        let center = top + height / 2.0;
        let mut text_left = left + pad;
        if let Some(badge) = badge {
            let side = height * 0.5;
            let badge_width = side * badge.aspect.min(1.0);
            let badge_height = side / badge.aspect.max(1.0);
            page.image(
                badge.index,
                [
                    text_left,
                    center - badge_height / 2.0,
                    badge_width,
                    badge_height,
                ],
            )?;
            text_left += side + (5.0 * self.scale).max(3.0);
        }
        let text_width = left + width - pad - text_left;
        if node.subtitle.is_empty() {
            page.label(
                [text_left, center + name.size * 0.35],
                text_width,
                name,
                &node.title,
            )
        } else {
            let gap = address.size + 2.0;
            let baseline = center - gap * 0.35;
            page.label([text_left, baseline], text_width, name, &node.title)?;
            page.label(
                [text_left, baseline + gap],
                text_width,
                address,
                &node.subtitle,
            )
        }
    }
}

fn color_bytes(color: Color) -> [u8; 3] {
    [color.r, color.g, color.b].map(|value| (value * 255.0).round() as u8)
}

fn growth(scale: f32) -> f32 {
    ((scale - MIN_SCALE) / (1.0 - MIN_SCALE)).clamp(0.0, 1.0)
}

fn edge_stroke(kind: EdgeKind, scale: f32) -> VectorStroke {
    let jump = kind == EdgeKind::Jump;
    VectorStroke {
        color: if jump { JUMP } else { TUNNEL },
        width: 1.0 + 0.8 * growth(scale),
        dash: Some([
            if jump {
                (7.0 * scale).max(3.0)
            } else {
                (2.0 * scale).max(0.5)
            },
            (5.0 * scale).max(2.5),
        ]),
        round_caps: !jump,
    }
}

fn edge_points(from: Rectangle, to: Rectangle) -> [[f32; 2]; 4] {
    let dx = to.center_x() - from.center_x();
    let y0 = from.center_y();
    let y1 = to.center_y();
    let (x0, x1, c0, c1) = if dx.abs() < from.width {
        let x0 = from.x + from.width;
        let x1 = to.x + to.width;
        let bend = 36.0 + (y1 - y0).abs() / 6.0;
        (x0, x1, x0 + bend, x1 + bend)
    } else if dx > 0.0 {
        let x0 = from.x + from.width;
        let x1 = to.x;
        let bend = ((x1 - x0) / 2.2).max(40.0);
        (x0, x1, x0 + bend, x1 - bend)
    } else {
        let x0 = from.x;
        let x1 = to.x + to.width;
        let bend = ((x0 - x1) / 2.2).max(40.0);
        (x0, x1, x0 - bend, x1 + bend)
    };
    [[x0, y0], [c0, y0], [c1, y1], [x1, y1]]
}

struct TunnelList {
    pages: Vec<Vec<(f32, String)>>,
    left: f32,
    width: f32,
    style: Style,
}

impl TunnelList {
    fn new(pdf: &Pdf<'_>, plan: &Plan, bottom: f32) -> Self {
        let scale = plan.scale;
        let computer = plan.layout.nodes[0].bounds;
        let left = computer.x * scale;
        let width = computer.width * scale;
        let size = 6.5 + 2.0 * growth(scale);
        let style = Style::new(Font::Regular, size, TUNNEL_TEXT);
        let leading = size * 1.3;
        let new_page_top = HEADER + size * 0.85;
        let mut baseline = new_page_top + (computer.y + computer.height + 12.0) * scale;
        let mut pages = Vec::new();
        let mut rows = Vec::new();
        for edge in plan
            .layout
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Tunnel)
        {
            let text = format!("{}: {}", plan.layout.nodes[edge.to].title, edge.label);
            let lines = pdf.wrap_text(width, style, &text);
            if baseline + (lines.len().saturating_sub(1) as f32) * leading > bottom
                && !rows.is_empty()
            {
                pages.push(std::mem::take(&mut rows));
                baseline = new_page_top;
            }
            for line in lines {
                if baseline > bottom && !rows.is_empty() {
                    pages.push(std::mem::take(&mut rows));
                    baseline = new_page_top;
                }
                rows.push((baseline, line));
                baseline += leading;
            }
            baseline += size * 0.6;
        }
        if !rows.is_empty() {
            pages.push(rows);
        }
        Self {
            pages,
            left,
            width,
            style,
        }
    }
}
