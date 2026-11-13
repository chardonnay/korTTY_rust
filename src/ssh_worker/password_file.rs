//! The fixed privileged file helper shared by direct and isolated SSH.

use std::io::{self, Read};
use std::sync::OnceLock;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, bufread::ZlibEncoder};

pub const HELPER: &str = include_str!("../sftp_window/privileged_edit/protocol.py");
pub const HELLO: &[u8] = b"\0\0\0\x1f{\"status\":\"hello\",\"protocol\":1}";
pub const DEADLINE: Duration = Duration::from_secs(60);
pub const TRANSFER_DEADLINE: Duration = Duration::from_secs(60 * 60);

pub fn command(marker: Option<&str>) -> io::Result<String> {
    helper_command(marker, "")
}

pub fn transfer_command(marker: Option<&str>) -> io::Result<String> {
    helper_command(marker, " transfer")
}

fn helper_command(marker: Option<&str>, suffix: &str) -> io::Result<String> {
    let mut command = String::from("exec /usr/bin/sudo -k ");
    if let Some(marker) = marker {
        command.push_str("-S -p ");
        quote(&mut command, marker);
        command.push(' ');
    } else {
        command.push_str("-n ");
    }
    command.push_str("-u root -- /usr/bin/python3 -I -S -c ");
    quote(&mut command, python_command()?);
    command.push_str(suffix);
    if command.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Privileged file helper exceeds the SSH command limit",
        ));
    }
    Ok(command)
}

fn python_command() -> io::Result<&'static str> {
    static COMMAND: OnceLock<io::Result<String>> = OnceLock::new();
    COMMAND
        .get_or_init(|| {
            let mut compressed = Vec::new();
            ZlibEncoder::new(HELPER.as_bytes(), Compression::fast()).read_to_end(&mut compressed)?;
            Ok(format!(
                "import base64,zlib;exec(compile(zlib.decompress(base64.b64decode(\"{}\")),\"<kortty-file-helper>\",\"exec\"))",
                STANDARD.encode(compressed)
            ))
        })
        .as_deref()
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

fn quote(output: &mut String, value: &str) {
    output.push('\'');
    output.push_str(&value.replace('\'', "'\\''"));
    output.push('\'');
}

#[cfg(test)]
mod tests {
    use flate2::bufread::ZlibDecoder;

    use super::*;

    #[test]
    fn privileged_file_command_encodes_the_exact_bounded_helper() {
        let script = python_command().unwrap();
        let encoded = script.split('"').nth(1).unwrap();
        let compressed = STANDARD.decode(encoded).unwrap();
        let mut unpacked = String::new();
        ZlibDecoder::new(compressed.as_slice())
            .read_to_string(&mut unpacked)
            .unwrap();
        assert_eq!(unpacked, HELPER);
        let marker = crate::ssh_runtime::password::marker([0xff; 16]);
        let command = transfer_command(Some(&marker)).unwrap();
        assert!(command.len() < kortty_ssh::MAX_EXEC_COMMAND_BYTES);
        assert!(command.ends_with("' transfer"));
    }
}
