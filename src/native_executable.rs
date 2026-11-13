//! Native executable header checks shared by release packaging and downloads.

use std::io::{self, Read};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Linux,
    Windows,
    Macos,
}

#[derive(Clone, Copy)]
enum Architecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy)]
pub(crate) struct ExecutableTarget {
    pub(crate) platform: Platform,
    architecture: Architecture,
}

impl ExecutableTarget {
    pub(crate) fn parse(triple: &str) -> io::Result<Self> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let (architecture, os) = triple
            .split_once('-')
            .ok_or_else(|| invalid("expected a native Rust target triple".into()))?;
        let architecture = match architecture {
            "x86_64" => Architecture::X86_64,
            "aarch64" => Architecture::Aarch64,
            _ => return Err(invalid(format!("unsupported architecture in {triple}"))),
        };
        let platform = match os {
            "unknown-linux-gnu" => Platform::Linux,
            "pc-windows-msvc" | "pc-windows-gnullvm" => Platform::Windows,
            "pc-windows-gnu" if matches!(architecture, Architecture::X86_64) => Platform::Windows,
            "apple-darwin" => Platform::Macos,
            _ => return Err(invalid(format!("unsupported platform in {triple}"))),
        };
        Ok(Self {
            platform,
            architecture,
        })
    }

    pub(crate) fn matches(self, file: &mut impl Read) -> io::Result<bool> {
        let mut header = [0; 64];
        file.read_exact(&mut header)?;
        let (elf_machine, pe_machine, macho_machine) = match self.architecture {
            Architecture::X86_64 => (62, 0x8664, 0x0100_0007),
            Architecture::Aarch64 => (183, 0xaa64, 0x0100_000c),
        };
        match self.platform {
            Platform::Linux => Ok(header.starts_with(b"\x7fELF\x02\x01")
                && matches!(word(&header, 16), 2 | 3)
                && word(&header, 18) == elf_machine),
            Platform::Macos => Ok(dword(&header, 0) == 0xfeed_facf
                && dword(&header, 4) == macho_machine
                && dword(&header, 12) == 2),
            Platform::Windows => {
                const MAX_PE_OFFSET: u32 = 1024 * 1024;
                let pe_offset = dword(&header, 60);
                if !header.starts_with(b"MZ") || !(64..=MAX_PE_OFFSET).contains(&pe_offset) {
                    return Ok(false);
                }
                let skip = u64::from(pe_offset - 64);
                if io::copy(&mut (&mut *file).take(skip), &mut io::sink())? != skip {
                    return Ok(false);
                }
                let mut pe = [0; 26];
                file.read_exact(&mut pe)?;
                const EXECUTABLE: u16 = 0x0002;
                const DLL: u16 = 0x2000;
                Ok(pe.starts_with(b"PE\0\0")
                    && word(&pe, 4) == pe_machine
                    && word(&pe, 22) & (EXECUTABLE | DLL) == EXECUTABLE
                    && word(&pe, 24) == 0x20b)
            }
        }
    }
}

fn word(header: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([header[index], header[index + 1]])
}

fn dword(header: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(header[index..index + 4].try_into().expect("header word"))
}
