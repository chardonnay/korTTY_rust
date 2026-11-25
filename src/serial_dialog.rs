use std::fmt;
use std::path::PathBuf;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use kortty_terminal::{TerminalProfile, TextEncoding};

use crate::session::{
    SerialConnection, SerialFlowControl, SerialLineEnding, SerialParity, SerialSettings,
    SerialStopBits, available_serial_ports,
};

const BAUD_RATES: [u32; 12] = [
    300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600,
];
const PARITIES: [Choice<SerialParity>; 5] = [
    Choice(SerialParity::None, "No parity"),
    Choice(SerialParity::Odd, "Odd"),
    Choice(SerialParity::Even, "Even"),
    Choice(SerialParity::Mark, "Mark"),
    Choice(SerialParity::Space, "Space"),
];
const STOPS: [Choice<SerialStopBits>; 3] = [
    Choice(SerialStopBits::One, "1 stop bit"),
    Choice(SerialStopBits::OnePointFive, "1.5 stop bits"),
    Choice(SerialStopBits::Two, "2 stop bits"),
];
const FLOW: [Choice<SerialFlowControl>; 4] = [
    Choice(SerialFlowControl::None, "None"),
    Choice(SerialFlowControl::RtsCts, "RTS/CTS (hardware)"),
    Choice(SerialFlowControl::XonXoff, "XON/XOFF (software)"),
    Choice(SerialFlowControl::DsrDtr, "DSR/DTR"),
];
const ENDINGS: [Choice<SerialLineEnding>; 3] = [
    Choice(SerialLineEnding::Cr, "CR"),
    Choice(SerialLineEnding::Lf, "LF"),
    Choice(SerialLineEnding::CrLf, "CR LF"),
];
const ENCODINGS: [Choice<Option<TextEncoding>>; 5] = [
    Choice(None, "Use default"),
    Choice(Some(TextEncoding::Utf8), "UTF-8"),
    Choice(Some(TextEncoding::Latin1), "ISO-8859-1"),
    Choice(Some(TextEncoding::Latin9), "ISO-8859-15"),
    Choice(Some(TextEncoding::Windows1252), "Windows-1252"),
];

#[derive(Clone, Copy, PartialEq)]
struct Choice<T>(T, &'static str);

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.1)
    }
}

fn current<T: Copy + PartialEq>(options: &[Choice<T>], value: T) -> Option<Choice<T>> {
    options.iter().copied().find(|choice| choice.0 == value)
}

#[derive(Debug, Clone)]
pub enum Message {
    Focus(iced::widget::Id),
    Device(String),
    Baud(String),
    DataBits(u8),
    Parity(SerialParity),
    StopBits(SerialStopBits),
    Flow(SerialFlowControl),
    LineEnding(SerialLineEnding),
    Dtr(bool),
    Rts(bool),
    LocalEcho(bool),
    Encoding(Option<TextEncoding>),
    Profile(TerminalProfile),
    Refresh,
    Ports(u64, Result<Vec<String>, String>),
    Connect,
    Reconnect(u64),
    Cancel,
}

pub struct SerialDialog {
    id: u64,
    connection: SerialConnection,
    profile: TerminalProfile,
    device: String,
    baud: String,
    ports: Vec<String>,
    scanning: bool,
    scan_error: Option<String>,
    protocol_ids: [iced::widget::Id; 8],
    return_refresh_focus: bool,
}

impl SerialDialog {
    pub fn new(id: u64, connection: SerialConnection, profile: TerminalProfile) -> Self {
        Self {
            id,
            device: connection.device.to_string_lossy().into_owned(),
            baud: connection.settings.baud_rate.to_string(),
            connection,
            profile,
            ports: Vec::new(),
            scanning: false,
            scan_error: None,
            protocol_ids: std::array::from_fn(|_| iced::widget::Id::unique()),
            return_refresh_focus: false,
        }
    }

    pub fn refresh(&mut self) -> Task<Message> {
        if self.scanning {
            return Task::none();
        }
        self.scanning = true;
        self.scan_error = None;
        let id = self.id;
        Task::future(async move {
            let ports = available_serial_ports()
                .map(|paths| {
                    paths
                        .into_iter()
                        .filter_map(|path| path.into_os_string().into_string().ok())
                        .collect()
                })
                .map_err(|error| error.to_string());
            Message::Ports(id, ports)
        })
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Device(device) if device.len() <= 1024 => self.device = device,
            Message::Baud(baud)
                if baud.len() <= 8 && baud.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                self.baud = baud;
            }
            Message::DataBits(bits) => self.connection.settings.data_bits = bits,
            Message::Parity(parity) => self.connection.settings.parity = parity,
            Message::StopBits(bits) => self.connection.settings.stop_bits = bits,
            Message::Flow(flow) => self.connection.settings.flow_control = flow,
            Message::LineEnding(ending) => self.connection.line_ending = ending,
            Message::Dtr(assert) if self.dtr_available() => {
                self.connection.settings.assert_dtr = assert;
            }
            Message::Rts(assert) if self.rts_available() => {
                self.connection.settings.assert_rts = assert;
            }
            Message::LocalEcho(echo) => self.connection.local_echo = echo,
            Message::Encoding(encoding) if self.profile != TerminalProfile::Petscii => {
                self.connection.encoding = encoding;
            }
            Message::Profile(profile) => self.profile = profile,
            Message::Refresh => {
                self.return_refresh_focus = true;
                return self.refresh();
            }
            Message::Ports(id, result) if id == self.id => {
                self.scanning = false;
                match result {
                    Ok(ports) => self.ports = ports,
                    Err(error) => self.scan_error = Some(error),
                }
                if std::mem::take(&mut self.return_refresh_focus) {
                    return Task::done(Message::Focus(self.protocol_ids[7].clone()));
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub fn validated(&self) -> Result<(SerialConnection, TerminalProfile), String> {
        let (connection, profile) = self.settings()?;
        connection.validate().map_err(|error| error.to_string())?;
        Ok((connection, profile))
    }

    pub(crate) fn settings(&self) -> Result<(SerialConnection, TerminalProfile), String> {
        let mut connection = self.connection.clone();
        connection.device = PathBuf::from(self.device.trim());
        connection.settings.baud_rate = self
            .baud
            .parse()
            .map_err(|_| String::from("Enter a baud rate from 1 to 20,000,000."))?;
        kortty_serial::SerialPort::validate_settings(&connection.device, connection.settings)
            .map_err(|error| error.to_string())?;
        Ok((connection, self.profile))
    }

    pub(crate) fn load(&mut self, connection: SerialConnection, profile: TerminalProfile) {
        self.device = connection.device.to_string_lossy().into_owned();
        self.baud = connection.settings.baud_rate.to_string();
        self.connection = connection;
        self.profile = profile;
    }

    pub(crate) fn profile(&self) -> TerminalProfile {
        self.profile
    }

    fn dtr_available(&self) -> bool {
        self.connection.settings.flow_control != SerialFlowControl::DsrDtr
    }

    fn rts_available(&self) -> bool {
        self.connection.settings.flow_control != SerialFlowControl::RtsCts
    }

    pub(crate) fn editor_fields(&self) -> Element<'_, Message> {
        column![
            self.connection_fields(false, true, true, 110),
            labeled(
                "Emulation",
                pick_list(TerminalProfile::ALL, Some(self.profile), Message::Profile)
            ),
        ]
        .spacing(8)
        .into()
    }

    pub(crate) fn protocol_fields(&self, enabled: bool) -> Element<'_, Message> {
        self.protocol_fields_at(enabled, 148)
    }

    pub(crate) fn protocol_fields_at(
        &self,
        enabled: bool,
        label_width: u16,
    ) -> Element<'_, Message> {
        let settings = self.connection.settings;
        let port_menu: Element<'_, Message> = if enabled {
            pick_list(
                self.ports.as_slice(),
                Some(self.device.clone()),
                Message::Device,
            )
            .show_value(false)
            .keyboard_navigation(self.protocol_ids[0].clone())
            .on_open(Message::Focus(self.protocol_ids[0].clone()))
            .menu_width(350)
            .width(30)
            .text_size(13)
            .into()
        } else {
            button(text("▾").size(13))
                .width(30)
                .style(crate::workspace::dialog_design::secondary_button)
                .into()
        };
        let baud_menu: Element<'_, Message> = if enabled {
            pick_list(BAUD_RATES, self.baud.parse::<u32>().ok(), |value| {
                Message::Baud(value.to_string())
            })
            .show_value(false)
            .keyboard_navigation(self.protocol_ids[1].clone())
            .on_open(Message::Focus(self.protocol_ids[1].clone()))
            .menu_width(120)
            .width(30)
            .text_size(13)
            .into()
        } else {
            button(text("▾").size(13))
                .width(30)
                .style(crate::workspace::dialog_design::secondary_button)
                .into()
        };
        let port = row![
            row![
                text_input("COM3 or /dev/ttyUSB0", &self.device)
                    .style(crate::workspace::dialog_design::form_input)
                    .size(13)
                    .on_input_maybe(enabled.then_some(Message::Device))
                    .width(Length::Fill),
                port_menu
            ]
            .width(Length::Fill),
            crate::workspace::dialog_design::keyboard_table::action_with_id(
                button(
                    text(if self.scanning {
                        "Scanning…"
                    } else {
                        "Refresh"
                    })
                    .size(13)
                )
                .on_press_maybe((enabled && !self.scanning).then_some(Message::Refresh))
                .into(),
                (enabled && !self.scanning).then_some(Message::Refresh),
                true,
                Some(self.protocol_ids[7].clone())
            ),
        ]
        .spacing(8)
        .width(if label_width > 148 {
            Length::Fill
        } else {
            Length::Fixed(430.0)
        });
        let rate = row![
            text_input("Baud rate", &self.baud)
                .style(crate::workspace::dialog_design::form_input)
                .size(13)
                .width(90)
                .on_input_maybe(enabled.then_some(Message::Baud)),
            baud_menu
        ];
        let frame = row![
            protocol_choice(
                enabled,
                [8, 7, 6, 5],
                Some(settings.data_bits),
                Message::DataBits,
                55,
                self.protocol_ids[2].clone()
            ),
            protocol_choice(
                enabled,
                PARITIES,
                current(&PARITIES, settings.parity),
                |choice| Message::Parity(choice.0),
                100,
                self.protocol_ids[3].clone()
            ),
            protocol_choice(
                enabled,
                STOPS,
                current(&STOPS, settings.stop_bits),
                |choice| Message::StopBits(choice.0),
                110,
                self.protocol_ids[4].clone()
            ),
        ]
        .spacing(6)
        .wrap();
        let lines = row![
            protocol_checkbox(
                "Raise DTR on connect",
                settings.assert_dtr,
                enabled && self.dtr_available(),
                Message::Dtr
            ),
            protocol_checkbox(
                "Raise RTS on connect",
                settings.assert_rts,
                enabled && self.rts_available(),
                Message::Rts
            ),
            protocol_checkbox(
                "Local echo",
                self.connection.local_echo,
                enabled,
                Message::LocalEcho
            ),
        ]
        .spacing(10)
        .wrap();
        let status = enabled.then(|| self.scan_error.clone()).flatten();
        column![
            labeled_width("Serial port", port, label_width),
            labeled_width("Speed (baud)", rate, label_width),
            labeled_width("Data bits / parity / stop bits", frame, label_width),
            labeled_width(
                "Flow control",
                protocol_choice(
                    enabled,
                    FLOW,
                    current(&FLOW, settings.flow_control),
                    |choice| Message::Flow(choice.0),
                    170,
                    self.protocol_ids[5].clone()
                ),
                label_width
            ),
            labeled_width(
                "Enter sends",
                protocol_choice(
                    enabled,
                    ENDINGS,
                    current(&ENDINGS, self.connection.line_ending),
                    |choice| Message::LineEnding(choice.0),
                    100,
                    self.protocol_ids[6].clone()
                ),
                label_width
            ),
            labeled_width("Line options", lines, label_width),
            status.map(|status| labeled_width("", text(status).size(12), label_width)),
        ]
        .spacing(10)
        .into()
    }

    fn connection_fields(
        &self,
        submit: bool,
        compact: bool,
        encoding_field: bool,
        label_width: u16,
    ) -> Element<'_, Message> {
        let settings: SerialSettings = self.connection.settings;
        let validation = if compact {
            self.settings()
        } else {
            self.validated()
        };
        let connect = (submit && validation.is_ok()).then_some(Message::Connect);
        let selected = self.ports.iter().find(|port| *port == &self.device);
        let port_list = pick_list(self.ports.as_slice(), selected, Message::Device)
            .placeholder(if self.ports.is_empty() {
                "No ports listed"
            } else {
                "Detected ports"
            })
            .width(if compact { 135 } else { 175 });
        let device = text_input(
            if cfg!(windows) {
                "COM port"
            } else {
                "/dev/… device"
            },
            &self.device,
        )
        .on_input(Message::Device)
        .on_submit_maybe(connect.clone())
        .width(Length::Fill);
        let refresh = button(if self.scanning {
            "Scanning…"
        } else {
            "Refresh"
        })
        .on_press_maybe((!self.scanning).then_some(Message::Refresh));
        let port: Element<'_, Message> = if compact {
            column![device, row![port_list, refresh].spacing(6)]
                .spacing(6)
                .into()
        } else {
            row![device, port_list, refresh].spacing(6).into()
        };
        let rate = row![
            text_input("Baud rate", &self.baud)
                .on_input(Message::Baud)
                .width(105),
            pick_list(
                BAUD_RATES,
                self.baud
                    .parse::<u32>()
                    .ok()
                    .filter(|n| BAUD_RATES.contains(n)),
                |n| { Message::Baud(n.to_string()) }
            )
            .placeholder("Common rates")
        ]
        .spacing(6)
        .wrap();
        let frame = row![
            pick_list([8, 7, 6, 5], Some(settings.data_bits), Message::DataBits),
            pick_list(PARITIES, current(&PARITIES, settings.parity), |choice| {
                Message::Parity(choice.0)
            }),
            pick_list(STOPS, current(&STOPS, settings.stop_bits), |choice| {
                Message::StopBits(choice.0)
            }),
        ]
        .spacing(6)
        .wrap();
        let status = self.scan_error.as_ref().cloned().or_else(|| {
            (!self.device.trim().is_empty())
                .then(|| validation.err())
                .flatten()
        });
        let encoding: Element<'_, Message> = if self.profile == TerminalProfile::Petscii {
            text("PETSCII (native)").into()
        } else {
            pick_list(
                ENCODINGS,
                current(&ENCODINGS, self.connection.encoding),
                |choice| Message::Encoding(choice.0),
            )
            .into()
        };
        column![
            labeled_width("Device", port, label_width),
            labeled_width("Speed (baud)", rate, label_width),
            labeled_width("Frame", frame, label_width),
            labeled_width(
                "Flow control",
                pick_list(FLOW, current(&FLOW, settings.flow_control), |choice| {
                    Message::Flow(choice.0)
                }),
                label_width
            ),
            labeled_width(
                "Enter sends",
                pick_list(
                    ENDINGS,
                    current(&ENDINGS, self.connection.line_ending),
                    |choice| Message::LineEnding(choice.0)
                ),
                label_width
            ),
            encoding_field.then(|| labeled_width("Character encoding", encoding, label_width)),
            row![
                checkbox(settings.assert_dtr)
                    .label("Raise DTR on connect")
                    .on_toggle_maybe(self.dtr_available().then_some(Message::Dtr)),
                checkbox(settings.assert_rts)
                    .label("Raise RTS on connect")
                    .on_toggle_maybe(self.rts_available().then_some(Message::Rts)),
                checkbox(self.connection.local_echo)
                    .label("Local echo")
                    .on_toggle(Message::LocalEcho),
            ]
            .spacing(12)
            .wrap(),
            text(status.unwrap_or_default()).size(13),
        ]
        .spacing(8)
        .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        self.connection_view(None)
    }

    pub(crate) fn reconnect_view(&self, request: u64, disconnect: bool) -> Element<'_, Message> {
        self.connection_view(Some((request, disconnect)))
    }

    fn connection_view(&self, reconnect: Option<(u64, bool)>) -> Element<'_, Message> {
        let (title, label, action) = match reconnect {
            Some((request, disconnect)) => (
                "Reconnect serial connection",
                if disconnect {
                    "Disconnect and reconnect"
                } else {
                    "Reconnect"
                },
                Message::Reconnect(request),
            ),
            None => ("Serial connection", "Connect", Message::Connect),
        };
        let mut content = column![
            text(title).size(22),
            self.connection_fields(reconnect.is_none(), false, true, 110),
            labeled(
                "Emulation",
                pick_list(TerminalProfile::ALL, Some(self.profile), Message::Profile)
            ),
        ];
        if reconnect.is_some_and(|(_, disconnect)| disconnect) {
            content = content.push(text(
                "The current serial session must close before the port can reopen.",
            ));
        }
        let content = content
            .push(
                row![
                    button(label).on_press_maybe(self.validated().is_ok().then_some(action)),
                    button("Cancel").on_press(Message::Cancel)
                ]
                .spacing(8),
            )
            .spacing(8)
            .padding(16)
            .max_width(730);
        scrollable(container(content).width(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn protocol_checkbox(
    label: &'static str,
    checked: bool,
    enabled: bool,
    on_toggle: fn(bool) -> Message,
) -> Element<'static, Message> {
    crate::workspace::dialog_design::keyboard_table::action(
        checkbox(checked)
            .label(label)
            .text_size(13)
            .on_toggle_maybe(enabled.then_some(on_toggle))
            .into(),
        enabled.then(|| on_toggle(!checked)),
        false,
    )
}

fn protocol_choice<'a, T: Clone + PartialEq + fmt::Display + 'a>(
    enabled: bool,
    options: impl IntoIterator<Item = T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
    width: u16,
    id: iced::widget::Id,
) -> Element<'a, Message> {
    if enabled {
        pick_list(options.into_iter().collect::<Vec<_>>(), selected, on_select)
            .width(f32::from(width))
            .text_size(13)
            .keyboard_navigation(id.clone())
            .on_open(Message::Focus(id))
            .into()
    } else {
        button(row![
            text(selected.map_or_else(String::new, |value| value.to_string()))
                .size(13)
                .width(Length::Fill),
            text("▾").size(13)
        ])
        .width(f32::from(width))
        .style(crate::workspace::dialog_design::secondary_button)
        .into()
    }
}

fn labeled_width<'a>(
    label: &'a str,
    field: impl Into<Element<'a, Message>>,
    width: u16,
) -> Element<'a, Message> {
    row![
        text(label)
            .size(if width == 148 { 13 } else { 16 })
            .width(f32::from(width)),
        field.into()
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).width(110), field.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use iced::futures::{StreamExt, executor::block_on};

    #[test]
    fn only_requested_refresh_completion_restores_focus_and_stale_replies_do_not() {
        let mut dialog = SerialDialog::new(17, SerialConnection::default(), TerminalProfile::Xterm);
        let _ = dialog.refresh();
        assert!(
            iced_runtime::task::into_stream(dialog.update(Message::Ports(17, Ok(vec![]))))
                .is_none()
        );
        let _ = dialog.update(Message::Refresh);
        assert!(
            iced_runtime::task::into_stream(dialog.update(Message::Ports(16, Ok(vec![]))))
                .is_none()
        );
        assert!(dialog.scanning);
        let reply = dialog.update(Message::Ports(17, Ok(vec!["COM17".into()])));
        let mut actions = iced_runtime::task::into_stream(reply).unwrap();
        assert!(
            matches!(block_on(actions.next()), Some(iced_runtime::Action::Output(Message::Focus(id))) if id == dialog.protocol_ids[7])
        );
        assert!(!dialog.scanning);
        assert!(
            iced_runtime::task::into_stream(dialog.update(Message::Ports(17, Ok(vec![]))))
                .is_none()
        );
    }
}
