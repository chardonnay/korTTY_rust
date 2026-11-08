use std::env;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, invalid};

pub(crate) const UNIX_PROGRAMS: &[&str] = &["kortty", "kortty-cli"];
pub(crate) const MACOS_PROGRAMS: &[&str] = &["kortty", "kortty-cli", "kortty-ssh-worker"];
pub(crate) const WINDOWS_PROGRAMS: &[&str] =
    &["kortty.exe", "kortty-cli.exe", "kortty-ssh-worker.exe"];

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

pub(crate) struct Target {
    pub(crate) platform: Platform,
    architecture: Architecture,
}

impl Target {
    pub(crate) fn parse(triple: &str) -> io::Result<Self> {
        let (architecture, os) = triple
            .split_once('-')
            .ok_or_else(|| invalid("expected a native Rust target triple"))?;
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

    pub(crate) fn binaries(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let names = match self.platform {
            Platform::Windows => WINDOWS_PROGRAMS,
            Platform::Linux => UNIX_PROGRAMS,
            Platform::Macos => MACOS_PROGRAMS,
        };
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                let at_path = |error: io::Error| {
                    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
                };
                let mut binary = File::open(&path).map_err(at_path)?;
                if !binary.metadata()?.is_file() || !self.matches(&mut binary).map_err(at_path)? {
                    return Err(invalid(format!(
                        "{} does not match the requested target",
                        path.display()
                    ))
                    .into());
                }
                Ok(path)
            })
            .collect()
    }

    fn matches(&self, file: &mut File) -> io::Result<bool> {
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
                let pe_offset = dword(&header, 60);
                if !header.starts_with(b"MZ") || pe_offset < header.len() as u32 {
                    return Ok(false);
                }
                file.seek(SeekFrom::Start(u64::from(pe_offset)))?;
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
    u32::from_le_bytes([
        header[index],
        header[index + 1],
        header[index + 2],
        header[index + 3],
    ])
}

pub(crate) fn native() -> Result<String> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output()?;
    if !output.status.success() {
        return Err(io::Error::other("rustc -vV failed; specify --target").into());
    }
    let output = String::from_utf8(output.stdout)?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| invalid("rustc host target not found; specify --target").into())
}
