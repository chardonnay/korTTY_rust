use std::io::{self, Cursor, Read};
use std::path::Path;

use iced::widget::{button, column, image, row, scrollable, text};
use iced::{Element, Length, Task};

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::private_files::FileStamp;
use crate::profile::{LocalFileTool, projects};

use super::{EntryKind, Location, Message, SftpWindow, Side, worker};

pub(super) const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Picture {
    handle: image::Handle,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Picture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Picture {
    #[cfg(test)]
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn view<'a, M: Clone + 'a>(
        &self,
        zoom: f32,
        change_zoom: impl Fn(f32) -> M,
    ) -> Element<'a, M> {
        column![
            row![
                button("−").on_press(change_zoom(zoom / 1.25)),
                button(text(format!("{:.0}%", zoom * 100.0))).on_press(change_zoom(1.0)),
                button("+").on_press(change_zoom(zoom * 1.25)),
            ]
            .spacing(8),
            scrollable(
                image(self.handle.clone())
                    .width(self.width as f32 * zoom)
                    .height(self.height as f32 * zoom)
            )
            .direction(scrollable::Direction::Both {
                vertical: Default::default(),
                horizontal: Default::default(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

pub(crate) fn load(path: &Path) -> io::Result<Picture> {
    #[cfg(windows)]
    let path = {
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("Invalid local image path"))?;
        Path::new(path.strip_prefix(r"\\?\").unwrap_or(path))
    };
    let open = || projects::open_local_file(path);
    let file = open()?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    stamp.require_unchanged(&open()?)?;
    decode(&bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> io::Result<Picture> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(io::Error::other("Image file exceeds 16 MiB"));
    }
    let reader = || ::image::ImageReader::new(Cursor::new(bytes)).with_guessed_format();
    let (width, height) = reader()?.into_dimensions().map_err(io::Error::other)?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(io::Error::other("Image exceeds the supported pixel limit"));
    }
    let mut decoder = reader()?;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(96 * 1024 * 1024);
    decoder.limits(limits);
    let rgba = decoder.decode().map_err(io::Error::other)?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Picture {
        handle: image::Handle::from_rgba(width, height, rgba.into_raw()),
        width,
        height,
    })
}

pub(super) struct Preview {
    ticket: u64,
    side: Side,
    label: String,
    hidden: bool,
    picture: Option<Picture>,
    zoom: f32,
}

impl Preview {
    pub(super) fn loading(&self) -> bool {
        self.picture.is_none()
    }

    pub(super) fn remote(&self) -> bool {
        self.side == Side::Remote
    }
}

impl SftpWindow {
    pub(crate) fn revalidate_remote_images(&mut self, allowed: bool) {
        self.remote_images_allowed = allowed && self.editor_policy != RemoteEditorPolicy::Deny;
        if self.worker.as_ref().is_none_or(worker::Worker::is_closed) {
            self.invalidate_remote_image();
        } else if !self.remote_images_allowed {
            self.close_remote_image();
        }
    }

    pub(super) fn remote_image_loading(&self) -> bool {
        self.image
            .as_ref()
            .is_some_and(|preview| preview.remote() && preview.loading())
    }

    pub(super) fn showing_image(&self) -> bool {
        self.image.as_ref().is_some_and(|preview| !preview.hidden)
    }

    pub(super) fn close_remote_image(&mut self) {
        if self.image.as_ref().is_some_and(Preview::remote) {
            self.close_image();
        }
    }

    pub(super) fn invalidate_remote_image(&mut self) {
        if self.image.as_ref().is_some_and(Preview::remote) {
            self.image = None;
        }
    }

    fn image_location(&self, side: Side) -> Option<Location> {
        if self.image.is_some()
            || self.prompt.is_some()
            || self.comparison.is_some()
            || self.transport_busy()
            || (side == Side::Remote
                && (!self.remote_images_allowed
                    || self.worker.as_ref().is_none_or(worker::Worker::is_closed)))
        {
            return None;
        }
        let (directory, entry) = self.panel(side).selected()?;
        let extension = entry.display.rsplit_once('.')?.1;
        if entry.kind != EntryKind::File
            || !["png", "jpg", "jpeg", "gif", "bmp"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        {
            return None;
        }
        directory.path.with(&entry.name)
    }

    pub(super) fn can_open_image(&self, side: Side) -> bool {
        self.image_location(side).is_some()
    }

    pub(super) fn open_image(&mut self, side: Side) -> Task<Message> {
        let Some(location) = self.image_location(side) else {
            return Task::none();
        };
        let label = location.display();
        let remote_reply = match &location {
            Location::Remote(path) => {
                let Some(worker) = self.worker.as_ref() else {
                    return Task::none();
                };
                match worker.submit(worker::Operation::OpenProjectImage(path.clone())) {
                    Ok(reply) => Some(reply),
                    Err(error) => {
                        self.status = Some(format!("Could not open image ({error})"));
                        return Task::none();
                    }
                }
            }
            Location::Local(_) => None,
        };
        self.counter = self.counter.wrapping_add(1);
        let ticket = self.counter;
        self.image = Some(Preview {
            ticket,
            side,
            label,
            hidden: false,
            picture: None,
            zoom: 1.0,
        });
        Task::perform(
            async move {
                match location {
                    Location::Local(path) => load(&path).map_err(|error| error.to_string()),
                    Location::Remote(_) => {
                        let reply = remote_reply
                            .expect("remote image request has a worker reply")
                            .recv()
                            .await
                            .map_err(|_| "The SFTP connection closed.")?
                            .map_err(|error| error.message)?;
                        match reply.as_ref() {
                            worker::Reply::ProjectImageOpened(bytes) => {
                                decode(&bytes.0).map_err(|error| error.to_string())
                            }
                            _ => Err("Unexpected SFTP image response.".into()),
                        }
                    }
                }
            },
            move |result| Message::ImageLoaded(ticket, result),
        )
    }

    pub(super) fn image_loaded(
        &mut self,
        ticket: u64,
        result: Result<Picture, String>,
    ) -> Task<Message> {
        let Some(mut preview) = self.image.take_if(|preview| preview.ticket == ticket) else {
            return Task::none();
        };
        let current = !preview.remote()
            || (self.remote_images_allowed
                && self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_closed()));
        if current && !preview.hidden {
            match result {
                Ok(picture) => {
                    preview.picture = Some(picture);
                    self.image = Some(preview);
                }
                Err(error) => self.status = Some(format!("Could not open image ({error})")),
            }
        }
        self.start_next_transfer()
    }

    pub(super) fn close_image(&mut self) {
        if let Some(preview) = &mut self.image {
            if preview.loading() {
                preview.hidden = true;
            } else {
                self.image = None;
            }
        }
    }

    pub(super) fn zoom_image(&mut self, zoom: f32) {
        if let Some(preview) = &mut self.image
            && preview.picture.is_some()
            && zoom.is_finite()
        {
            preview.zoom = zoom.clamp(LocalFileTool::MIN_ZOOM, LocalFileTool::MAX_ZOOM);
        }
    }

    pub(super) fn image_view(&self) -> Option<Element<'_, Message>> {
        let preview = self.image.as_ref().filter(|preview| !preview.hidden)?;
        let content: Element<'_, Message> = preview.picture.as_ref().map_or_else(
            || text("Opening image…").into(),
            |picture| picture.view(preview.zoom, Message::ImageZoom),
        );
        Some(
            column![
                button("Back to files").on_press(Message::CloseImage),
                text(&preview.label).size(13),
                content,
            ]
            .spacing(8)
            .padding(12)
            .height(Length::Fill)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use iced::futures::{StreamExt as _, executor::block_on};

    use super::*;
    use crate::sftp_window::{EntryName, files};

    #[test]
    fn sftp_browser_opens_local_image_without_transfer_or_remote_editor_access() {
        let profile = crate::snippets::tests::Profile::new();
        ::image::RgbaImage::new(3, 2)
            .save(profile.0.join("preview.PNG"))
            .unwrap();
        let target = serde_json::from_value(serde_json::json!({
            "host": "localhost", "port": 22, "username": "fixture"
        }))
        .unwrap();
        let mut browser = SftpWindow::new(target, None, false, RemoteEditorPolicy::Deny);
        browser
            .local
            .load(Arc::new(files::list_local(&profile.0).unwrap()));
        let _ = browser.update(Message::Select(
            Side::Local,
            EntryName::Local("preview.PNG".into()),
        ));
        assert!(browser.can_open_image(Side::Local));
        let mut task =
            iced_runtime::task::into_stream(browser.update(Message::OpenImage(Side::Local)))
                .expect("local image load task");
        let Some(iced_runtime::Action::Output(message @ Message::ImageLoaded(..))) =
            block_on(task.next())
        else {
            panic!("image load returned no result");
        };
        let _ = browser.update(message);
        let preview = browser.image.as_ref().expect("local image preview");
        assert_eq!(preview.picture.as_ref().unwrap().dimensions(), (3, 2));
        assert!(!preview.remote());
        let _ = browser.update(Message::ImageZoom(20.0));
        assert_eq!(
            browser.image.as_ref().unwrap().zoom,
            LocalFileTool::MAX_ZOOM
        );
        let _ = browser.update(Message::CloseImage);
        assert!(!browser.showing_image());
    }
}
