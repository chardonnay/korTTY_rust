use std::sync::LazyLock;

use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Layout, Shell, layout, overlay, renderer};
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, space, svg, text, text_input, tooltip,
};
use iced::{Alignment, Element, Event, Length, Rectangle, Renderer, Size, Theme, mouse, window};
use jiff::{Timestamp, tz::TimeZone};

use crate::session::SessionId;
use crate::sftp_window::{ConflictChoice, Entry, EntryKind, Location};

use super::{
    Action, Binding, Desktop, Dock, Message, PAGE, RemoteSidebarPosition, Tab, Window, Workspace,
};
use crate::workspace::{local_dock, tab_titles::REMOTE_SIDEBAR_MIN_WIDTH, transparency};

const TERMINAL_RESERVE: f32 = 260.0;
const MAX_CRUMBS: usize = 12;

impl Workspace {
    fn remote_dock_source(
        &self,
        owner: window::Id,
        tab: &Tab,
    ) -> Option<(local_dock::Side, SessionId, bool)> {
        let side = match self.window_settings.remote_sidebar_position {
            RemoteSidebarPosition::Left => local_dock::Side::Left,
            RemoteSidebarPosition::Right => local_dock::Side::Right,
            RemoteSidebarPosition::Hidden => return None,
        };
        if self.terminal_only_fullscreen_active(owner) {
            return None;
        }
        let binding = tab.remote_files.binding.as_ref();
        let session = binding
            .map(|binding| binding.browser.session())
            .or_else(|| self.preferred_remote_pane(tab))?;
        let manager = binding.is_some_and(|binding| binding.manager_loan.is_some())
            || self.remote_manager_owns(session);
        (binding.is_some() || manager).then_some((side, session, manager))
    }

    pub(in crate::workspace) fn visible_remote_dock_width(
        &self,
        owner: window::Id,
        tab: &Tab,
    ) -> f32 {
        self.remote_dock_source(owner, tab).map_or(0.0, |_| {
            f32::from(self.window_settings.remote_sidebar_width)
        })
    }

    pub(in crate::workspace) fn remote_dock_view<'a>(
        &'a self,
        owner: window::Id,
        window: &'a Window,
        tab: &'a Tab,
        panes: Element<'a, Desktop>,
        fitted_width: Option<f32>,
    ) -> Element<'a, Desktop> {
        let position = self.window_settings.remote_sidebar_position;
        let Some((side, session, manager)) = self.remote_dock_source(owner, tab) else {
            return panes;
        };
        let binding = tab.remote_files.binding.as_ref();
        let eligible = self.remote_pane(session).is_some();
        let transfer_allowed = self.clipboard.policy.file_transfer_allowed;
        let available = window.content_bounds().width;
        let (_, remaining) =
            self.local_dock
                .tab_drop_coordinates(available, 0.0, f64::from(available));
        let preferred = f32::from(self.window_settings.remote_sidebar_width);
        let width = fitted_width.unwrap_or_else(|| {
            preferred
                .min((remaining as f32 - TERMINAL_RESERVE).max(f32::from(REMOTE_SIDEBAR_MIN_WIDTH)))
        });
        let tab_id = tab.id;
        let sidebar = binding
            .map_or_else(
                || manager_placeholder(width),
                |binding| {
                    #[cfg(any(target_os = "linux", target_os = "windows"))]
                    let desktop_drag = self
                        .sidebar_drag_available(owner, tab_id, binding)
                        .then(|| tab.remote_files.desktop_drag_controls(tab_id, binding))
                        .flatten();
                    tab.remote_files.sidebar(
                        binding,
                        width,
                        eligible,
                        transfer_allowed,
                        manager,
                        #[cfg(any(target_os = "linux", target_os = "windows"))]
                        desktop_drag,
                    )
                },
            )
            .map(move |action| Desktop::RemoteDock(owner, Message::Action(tab_id, action)));
        let sidebar = Element::new(DropSurface {
            child: sidebar,
            owner,
            tab: tab_id,
            stamp: binding
                .filter(|binding| {
                    transfer_allowed
                        && eligible
                        && binding.browser.connected()
                        && binding.browser.path().is_some()
                        && !binding.browser.busy()
                        && !manager
                })
                .map(|binding| (binding.generation, binding.listing)),
        });
        let resize_from = fitted_width.map_or(width, |_| preferred);
        let divider =
            local_dock::dock_divider(side, resize_from).map(move |message| match message {
                local_dock::Message::Resize(_, width) => {
                    Desktop::RemoteDock(owner, Message::Resize(position, width))
                }
                local_dock::Message::FinishResize(_) => {
                    Desktop::RemoteDock(owner, Message::FinishResize(position))
                }
                _ => unreachable!("the dock divider emits resize messages only"),
            });
        let empty = || -> Element<'a, Desktop> { space::Space::new().into() };
        let (left, right) = match side {
            local_dock::Side::Left => (
                container(row![sidebar, divider])
                    .style(transparency::opaque_chrome)
                    .into(),
                empty(),
            ),
            local_dock::Side::Right => (
                empty(),
                container(row![divider, sidebar])
                    .style(transparency::opaque_chrome)
                    .into(),
            ),
        };
        row![left, panes, right]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn manager_placeholder(width: f32) -> Element<'static, Action> {
    let header = row![
        text("Remote files").size(15),
        space::horizontal(),
        icon_button(Glyph::Close, "Hide Remote Files", Some(Action::Hide), false),
    ]
    .align_y(Alignment::Center);
    container(column![header, text("SFTP Manager is using this pane.").size(12),].spacing(5))
        .padding(6)
        .style(container::bordered_box)
        .width(width)
        .height(Length::Fill)
        .into()
}

impl Dock {
    fn sidebar<'a>(
        &'a self,
        binding: &'a Binding,
        width: f32,
        eligible: bool,
        transfer_allowed: bool,
        manager: bool,
        #[cfg(any(target_os = "linux", target_os = "windows"))] desktop_drag: Option<(
            Element<'a, Action>,
            Option<&'a str>,
        )>,
    ) -> Element<'a, Action> {
        let browser = &binding.browser;
        let connected = eligible && browser.connected() && !manager;
        let ready = connected && browser.path().is_some() && !browser.busy();
        let selected = !self.selected.is_empty()
            && browser
                .entries()
                .iter()
                .filter(|entry| entry.kind.transferable() && self.selected.contains(&entry.name))
                .count()
                == self.selected.len();
        let header = row![
            text("Remote files").size(15),
            space::horizontal(),
            icon_button(
                Glyph::Pin,
                if binding.follow.pinned {
                    "Unpin folder"
                } else {
                    "Pin folder"
                },
                (!manager && eligible).then_some(Action::Pin),
                binding.follow.pinned
            ),
            icon_button(
                Glyph::Refresh,
                "Refresh",
                (!manager && eligible).then_some(Action::Refresh),
                false
            ),
            icon_button(
                Glyph::Upload,
                "Upload",
                (ready && transfer_allowed)
                    .then_some(Action::Upload(binding.generation, binding.listing)),
                false
            ),
            icon_button(
                Glyph::Download,
                "Download",
                (ready && selected && transfer_allowed)
                    .then_some(Action::Download(binding.generation, binding.listing)),
                false
            ),
            icon_button(
                Glyph::Manager,
                "Open in SFTP Manager",
                (manager || (connected && browser.path().is_some()))
                    .then_some(Action::OpenManager(binding.generation, binding.listing)),
                false
            ),
            icon_button(Glyph::Close, "Hide Remote Files", Some(Action::Hide), false),
        ]
        .spacing(2)
        .align_y(Alignment::Center)
        .wrap();

        let path = browser.path_display().unwrap_or_else(|| "~".into());
        let banner = if manager {
            "SFTP Manager is using this pane.".into()
        } else if binding.follow.foreign {
            "Paused while the shell shows another session.".into()
        } else if !connected {
            "The pane's SFTP channel is unavailable.".into()
        } else if binding.follow.pinned {
            format!("Pinned · {path}")
        } else {
            format!("Following the SSH pane · {path}")
        };
        let mut panel = column![header, text(banner).size(12)].spacing(5);
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some((control, error)) = desktop_drag {
            panel = panel.push(control);
            if let Some(error) = error {
                panel = panel.push(text(error).size(12));
            }
        }
        if let Some(path) = browser.path() {
            panel = panel.push(breadcrumbs(path, binding, connected));
        }
        let filter = self.filter.trim().to_lowercase();
        let first = self.page.saturating_mul(PAGE);
        let time_zone = TimeZone::system();
        let full_time = width >= 310.0;
        let mut table = column![].width(Length::Fill).spacing(1);
        let mut count = 0;
        for entry in browser.entries() {
            if !filter.is_empty() && !entry.display.to_lowercase().contains(&filter) {
                continue;
            }
            if count >= first && count < first.saturating_add(PAGE) {
                table = table.push(file_row(
                    entry,
                    binding,
                    connected,
                    self.selected.contains(&entry.name),
                    &time_zone,
                    full_time,
                ));
            }
            count += 1;
        }
        if count == 0 && browser.path().is_some() {
            table = table.push(text("No matching entries.").size(12));
        }
        panel = panel
            .push(
                text_input("Filter names", &self.filter)
                    .on_input_maybe((!manager).then_some(Action::Filter)),
            )
            .push(columns(
                text("Name").size(12),
                text("Size").size(12),
                text("Modified").size(12),
                full_time,
            ))
            .push(scrollable(table).width(Length::Fill).height(Length::Fill));
        let more = count > first.saturating_add(PAGE);
        if self.page != 0 || more {
            panel = panel.push(
                row![
                    button("Previous").on_press_maybe(
                        (!manager && self.page != 0).then_some(Action::Page(false))
                    ),
                    text(format!("Page {}", self.page + 1)).size(12),
                    button("Next").on_press_maybe((!manager && more).then_some(Action::Page(true))),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }
        if let Some(status) = browser.progress() {
            panel = panel.push(text(status).size(12));
        } else if let Some(status) = browser.status() {
            panel = panel.push(text(status).size(12));
        }
        if let Some(conflict) = browser.conflict()
            && let Some(request) = browser.conflict_revision()
        {
            let may_resolve = connected && transfer_allowed;
            panel = panel.push(text(conflict.description()).size(12)).push(
                row![
                    button("Replace").on_press_maybe(
                        (may_resolve && conflict.can_replace()).then_some(Action::Resolve(
                            binding.generation,
                            request,
                            ConflictChoice::Replace
                        ))
                    ),
                    button("Keep both").on_press_maybe(may_resolve.then_some(Action::Resolve(
                        binding.generation,
                        request,
                        ConflictChoice::KeepBoth
                    ))),
                    button("Skip").on_press_maybe(may_resolve.then_some(Action::Resolve(
                        binding.generation,
                        request,
                        ConflictChoice::Skip
                    ))),
                ]
                .spacing(4)
                .wrap(),
            );
        }
        container(panel)
            .padding(6)
            .style(container::bordered_box)
            .width(width)
            .height(Length::Fill)
            .into()
    }
}

fn breadcrumbs<'a>(path: &[u8], binding: &Binding, enabled: bool) -> Element<'a, Action> {
    let mut crumbs = row![crumb("/".into(), b"/".to_vec(), binding, enabled)].spacing(2);
    let components: Vec<_> = path
        .split(|&byte| byte == b'/')
        .filter(|part| !part.is_empty())
        .collect();
    let skip = components.len().saturating_sub(MAX_CRUMBS);
    if skip != 0 {
        crumbs = crumbs.push(text("…").size(12));
    }
    let mut prefix = Vec::with_capacity(path.len());
    for (index, part) in components.into_iter().enumerate() {
        prefix.push(b'/');
        prefix.extend_from_slice(part);
        if index >= skip {
            let label = Location::Remote(part.to_vec()).display();
            crumbs = crumbs.push(crumb(label, prefix.clone(), binding, enabled));
        }
    }
    crumbs.wrap().into()
}

fn crumb<'a>(
    label: String,
    path: Vec<u8>,
    binding: &Binding,
    enabled: bool,
) -> Element<'a, Action> {
    button(text(label).size(12))
        .style(button::text)
        .padding([1, 3])
        .on_press_maybe(enabled.then_some(Action::Browse(
            binding.generation,
            binding.listing,
            path,
        )))
        .into()
}

fn file_row<'a>(
    entry: &'a Entry,
    binding: &Binding,
    enabled: bool,
    selected: bool,
    zone: &TimeZone,
    full_time: bool,
) -> Element<'a, Action> {
    let mark = match entry.kind {
        EntryKind::Directory => "▸",
        EntryKind::File => "·",
        EntryKind::Link => "↪",
        EntryKind::Other => "–",
    };
    let size = entry.size.map_or_else(|| "—".into(), format_size);
    let modified = entry
        .modified
        .and_then(|seconds| Timestamp::from_second(i64::from(seconds)).ok())
        .map_or_else(
            || "—".into(),
            |timestamp| {
                let format = if full_time {
                    "%Y-%m-%d %H:%M"
                } else {
                    "%Y-%m-%d"
                };
                timestamp
                    .to_zoned(zone.clone())
                    .strftime(format)
                    .to_string()
            },
        );
    let content = container(columns(
        text(format!("{mark} {}", entry.display)).size(12),
        text(size).size(11),
        text(modified).size(11),
        full_time,
    ))
    .width(Length::Fill)
    .padding([2, 3])
    .style(if selected {
        selected_row
    } else {
        container::transparent
    });
    let mut input = mouse_area(content);
    if enabled {
        input = input
            .on_press(Action::Select(
                binding.generation,
                binding.listing,
                entry.name.clone(),
            ))
            .interaction(mouse::Interaction::Pointer);
        if entry.kind == EntryKind::Directory {
            input = input.on_double_click(Action::Open(
                binding.generation,
                binding.listing,
                entry.name.clone(),
            ));
        }
    }
    input.into()
}

fn columns<'a>(
    name: iced::widget::Text<'a>,
    size: iced::widget::Text<'a>,
    date: iced::widget::Text<'a>,
    full_time: bool,
) -> Element<'a, Action> {
    row![
        name.width(Length::Fill),
        size.width(52),
        date.width(if full_time { 105 } else { 72 }),
    ]
    .spacing(3)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .into()
}

fn selected_row(theme: &Theme) -> container::Style {
    let colors = theme.extended_palette().primary.weak;
    container::Style::default()
        .background(colors.color)
        .color(colors.text)
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        let exponent = (bytes.ilog2() / 10).min(6);
        let unit = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"][exponent as usize];
        format!(
            "{:.1} {unit}",
            bytes as f64 / (1u64 << (exponent * 10)) as f64
        )
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Pin,
    Refresh,
    Upload,
    Download,
    Manager,
    Close,
}

fn icon_button(
    glyph: Glyph,
    label: &'static str,
    action: Option<Action>,
    selected: bool,
) -> Element<'static, Action> {
    const PATHS: [&str; 6] = [
        "M9 3h6l-1 5 3 3v2H7v-2l3-3-1-5M12 13v7",
        "M19 8a8 8 0 1 0 1 7M19 3v5h-5",
        "M12 17V4m-5 5 5-5 5 5M5 17v3h14v-3",
        "M12 4v13m-5-5 5 5 5-5M5 17v3h14v-3",
        "M13 5h6v6m0-6-8 8m7 0v6H5V6h6",
        "m6 6 12 12M18 6 6 18",
    ];
    static ICONS: LazyLock<[svg::Handle; 6]> = LazyLock::new(|| {
        PATHS.map(|path| {
        svg::Handle::from_memory(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" d="{path}"/></svg>"#
        ).into_bytes())
    })
    });
    let enabled = action.is_some();
    let icon = svg::Svg::new(ICONS[glyph as usize].clone())
        .width(16)
        .height(16)
        .style(move |theme: &Theme, _| {
            let palette = theme.extended_palette();
            let mut color = if selected {
                palette.primary.base.text
            } else {
                palette.background.base.text
            };
            if !enabled {
                color.a *= 0.45;
            }
            svg::Style { color: Some(color) }
        });
    tooltip(
        button(icon)
            .padding(3)
            .style(if selected {
                button::primary
            } else {
                button::text
            })
            .on_press_maybe(action),
        container(text(label).size(12))
            .padding(4)
            .style(container::bordered_box),
        tooltip::Position::Bottom,
    )
    .into()
}

struct DropSurface<'a> {
    child: Element<'a, Desktop>,
    owner: window::Id,
    tab: u64,
    stamp: Option<(u64, u64)>,
}

impl Widget<Desktop, Theme, Renderer> for DropSurface<'_> {
    fn tag(&self) -> tree::Tag {
        self.child.as_widget().tag()
    }
    fn state(&self) -> tree::State {
        self.child.as_widget().state()
    }
    fn children(&self) -> Vec<Tree> {
        self.child.as_widget().children()
    }
    fn diff(&self, tree: &mut Tree) {
        self.child.as_widget().diff(tree);
    }
    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.child.as_widget_mut().layout(tree, renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.child
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Desktop>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::FileHovered(_) | window::Event::FileDropped(_)) = event
            && !shell.is_event_captured()
            && cursor
                .position_over(layout.bounds())
                .is_some_and(|point| viewport.contains(point))
        {
            if let Some((generation, listing)) = self.stamp
                && let Event::Window(window::Event::FileDropped(path)) = event
            {
                shell.publish(Desktop::RemoteDock(
                    self.owner,
                    Message::Action(self.tab, Action::Dropped(generation, listing, path.clone())),
                ));
            }
            // A rejected drop on the dock must never be interpreted as a terminal drop.
            shell.capture_event();
            return;
        }
        self.child.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.child
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.child
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<overlay::Element<'a, Desktop, Theme, Renderer>> {
        self.child
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
