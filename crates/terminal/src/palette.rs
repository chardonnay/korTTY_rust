/// The 16 configurable ANSI colors and the standard extended xterm palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    ansi: [[u8; 3]; 16],
}

impl Palette {
    pub const fn new(ansi: [[u8; 3]; 16]) -> Self {
        Self { ansi }
    }

    pub fn rgb(&self, index: u8) -> [u8; 3] {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let index = index - 16;
                let steps = [0, 95, 135, 175, 215, 255];
                [
                    steps[usize::from(index / 36)],
                    steps[usize::from(index / 6 % 6)],
                    steps[usize::from(index % 6)],
                ]
            }
            _ => [8 + (index - 232) * 10; 3],
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        // SithTermFX v1.2.7 ColorPaletteImpl, using its Windows console palette on Windows.
        let normal = if cfg!(windows) {
            [
                0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0,
            ]
        } else {
            [
                0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x1e90ff, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            ]
        };
        let gray = if cfg!(windows) { 0x808080 } else { 0x4c4c4c };
        let bright = [
            gray, 0xff0000, 0x00ff00, 0xffff00, 0x4682b4, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        Self::new(std::array::from_fn(|index| {
            let hex = if index < 8 {
                normal[index]
            } else {
                bright[index - 8]
            };
            [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
        }))
    }
}
