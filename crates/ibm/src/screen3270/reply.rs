use super::{Cell, Color3270, Error3270, Field, Highlight3270, Style3270, Tn3270Screen, next};

const SBA: u8 = 0x11;
const SF: u8 = 0x1d;
const SA: u8 = 0x28;
const SFE: u8 = 0x29;
const ATTRIBUTE_TYPES: [u8; 3] = [0x41, 0x42, 0x45];

// IBM GA23-0059-07, Appendix C. Maps the six data bits to an EBCDIC graphic.
const GRAPHIC_ADDRESS: [u8; 64] = [
    0x40, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
    0x50, 0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
    0x60, 0x61, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f,
    0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f,
];

#[derive(Clone, Copy, Default)]
pub(super) enum ReplyMode {
    #[default]
    Field,
    ExtendedField,
    Character([bool; ATTRIBUTE_TYPES.len()]),
}

impl ReplyMode {
    pub(super) fn character(attributes: &[u8]) -> Result<Self, Error3270> {
        let mut enabled = [false; ATTRIBUTE_TYPES.len()];
        for &attribute in attributes {
            let index = ATTRIBUTE_TYPES
                .iter()
                .position(|&kind| kind == attribute)
                .ok_or(Error3270::UnsupportedAttribute(attribute))?;
            enabled[index] = true;
        }
        Ok(Self::Character(enabled))
    }
}

impl Tn3270Screen {
    pub(super) fn read_buffer(&self, aid: u8) -> Vec<u8> {
        let mut reply = self.read_heading(aid);
        let mut previous = [0; ATTRIBUTE_TYPES.len()];
        for &cell in &self.cells {
            match cell {
                Cell::Field(field) => self.push_field(&mut reply, field),
                Cell::Character { byte, style } => {
                    self.push_character(&mut reply, byte, style, &mut previous);
                }
            }
        }
        reply
    }

    pub(super) fn read_modified(&self, aid: u8, all: bool) -> Vec<u8> {
        if !all && matches!(aid, 0x6b..=0x6e) {
            return vec![aid];
        }
        let mut reply = self.read_heading(aid);
        let mut previous = [0; ATTRIBUTE_TYPES.len()];
        let mut formatted = false;
        for (address, &cell) in self.cells.iter().enumerate() {
            let Cell::Field(field) = cell else {
                continue;
            };
            formatted = true;
            if field.modified() {
                let mut field_address = next(address);
                reply.push(SBA);
                reply.extend(encode_address(field_address));
                while let Cell::Character { byte, style } = self.cells[field_address] {
                    if byte != 0 {
                        self.push_character(&mut reply, byte, style, &mut previous);
                    }
                    field_address = next(field_address);
                }
            }
        }
        if !formatted {
            for &cell in &self.cells {
                if let Cell::Character { byte, style } = cell
                    && byte != 0
                {
                    self.push_character(&mut reply, byte, style, &mut previous);
                }
            }
        }
        reply
    }

    fn read_heading(&self, aid: u8) -> Vec<u8> {
        let [high, low] = encode_address(self.cursor);
        vec![aid, high, low]
    }

    fn push_field(&self, reply: &mut Vec<u8>, field: Field) {
        let basic = GRAPHIC_ADDRESS[usize::from(field.bits)];
        if matches!(self.reply_mode, ReplyMode::Field) {
            reply.extend([SF, basic]);
            return;
        }
        // IBM GA23-0059-07 §3.6.1.3 omits defaults in an inbound SFE.
        let count_index = reply.len() + 1;
        reply.extend([SFE, 0]);
        if field.bits != 0 {
            reply.extend([0xc0, basic]);
        }
        for (kind, value) in ATTRIBUTE_TYPES.into_iter().zip(style_values(field.style)) {
            if value != 0 {
                reply.extend([kind, value]);
            }
        }
        reply[count_index] = ((reply.len() - count_index - 1) / 2) as u8;
    }

    fn push_character(
        &self,
        reply: &mut Vec<u8>,
        byte: u8,
        style: Style3270,
        previous: &mut [u8; ATTRIBUTE_TYPES.len()],
    ) {
        if let ReplyMode::Character(enabled) = self.reply_mode {
            for (index, value) in style_values(style).into_iter().enumerate() {
                if enabled[index] && value != previous[index] {
                    reply.extend([SA, ATTRIBUTE_TYPES[index], value]);
                    previous[index] = value;
                }
            }
        }
        reply.push(byte);
    }
}

fn encode_address(address: usize) -> [u8; 2] {
    [
        GRAPHIC_ADDRESS[address >> 6],
        GRAPHIC_ADDRESS[address & 0x3f],
    ]
}

fn style_values(style: Style3270) -> [u8; ATTRIBUTE_TYPES.len()] {
    let highlighting = match style.highlighting {
        None => 0,
        Some(Highlight3270::Normal) => 0xf0,
        Some(Highlight3270::Blink) => 0xf1,
        Some(Highlight3270::Reverse) => 0xf2,
        Some(Highlight3270::Underline) => 0xf4,
    };
    [
        highlighting,
        color_value(style.foreground),
        color_value(style.background),
    ]
}

fn color_value(color: Option<Color3270>) -> u8 {
    use Color3270 as Color;
    match color {
        None => 0,
        Some(Color::Blue) => 0xf1,
        Some(Color::Red) => 0xf2,
        Some(Color::Pink) => 0xf3,
        Some(Color::Green) => 0xf4,
        Some(Color::Turquoise) => 0xf5,
        Some(Color::Yellow) => 0xf6,
        Some(Color::White) => 0xf7,
        Some(Color::Black) => 0xf8,
        Some(Color::DeepBlue) => 0xf9,
        Some(Color::Orange) => 0xfa,
        Some(Color::Purple) => 0xfb,
        Some(Color::PaleGreen) => 0xfc,
        Some(Color::PaleTurquoise) => 0xfd,
        Some(Color::Grey) => 0xfe,
    }
}
