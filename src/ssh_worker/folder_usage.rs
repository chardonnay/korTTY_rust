//! Fixed read-only folder usage command shared by direct and isolated SSH.

use std::io;
use std::time::Duration;

pub const MAX_PATHS: usize = 256;
pub const OUTPUT_LIMIT: usize = 1024 * 1024;
pub const DEADLINE: Duration = Duration::from_secs(10 * 60);

pub fn command(paths: &[Vec<u8>]) -> io::Result<String> {
    if !(1..=MAX_PATHS).contains(&paths.len()) {
        return Err(invalid("Choose up to 256 server folders to measure."));
    }
    let mut command = String::from("LC_ALL=C exec du -sk --");
    const SUFFIX: &str = " </dev/null";
    for path in paths {
        if path.len() > kortty_ssh::MAX_EXEC_COMMAND_BYTES {
            return Err(command_too_long());
        }
        let path = std::str::from_utf8(path)
            .map_err(|_| invalid("Allocated folder sizes require UTF-8 server paths."))?;
        let components = path
            .strip_prefix('/')
            .ok_or_else(|| invalid("Allocated folder sizes require absolute server paths."))?;
        if (!components.is_empty()
            && components
                .split('/')
                .any(|part| matches!(part, "" | "." | "..")))
            || path.chars().any(char::is_control)
        {
            return Err(invalid(
                "Allocated folder sizes require canonical server paths without control characters.",
            ));
        }
        let quote_bytes = path.bytes().filter(|&byte| byte == b'\'').count() * 3;
        if command.len() + 3 + path.len() + quote_bytes + SUFFIX.len()
            > kortty_ssh::MAX_EXEC_COMMAND_BYTES
        {
            return Err(command_too_long());
        }
        // Preserve argument order so du counts shared hard links once across the batch.
        command.push_str(" '");
        command.push_str(&path.replace('\'', "'\\''"));
        command.push('\'');
    }
    command.push_str(SUFFIX);
    Ok(command)
}

fn command_too_long() -> io::Error {
    invalid("The selected folder paths exceed the SSH command limit.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_usage_quotes_every_argument_in_one_ordered_batch() {
        let paths = [
            b"/first/folder space".to_vec(),
            b"/second/'$(echo x);*".to_vec(),
            "/third/ü".as_bytes().to_vec(),
            b"/".to_vec(),
        ];
        assert_eq!(
            command(&paths).unwrap(),
            "LC_ALL=C exec du -sk -- '/first/folder space' '/second/'\\''$(echo x);*' '/third/ü' '/' </dev/null"
        );
    }

    #[test]
    fn folder_usage_checks_argument_shape_and_escaped_command_size() {
        for path in [
            &b""[..],
            b"relative",
            b"-a",
            b"//a",
            b"/a/",
            b"/a/./b",
            b"/a/../b",
            b"/a\nb",
            b"/a\tb",
            b"/a\0b",
            b"/a\xff",
        ] {
            assert_eq!(
                command(&[path.to_vec()]).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert!(command(&[]).is_err());
        assert!(command(&vec![b"/a".to_vec(); MAX_PATHS + 1]).is_err());
        let overhead = command(&[b"/".to_vec()]).unwrap().len();
        let mut path = b"/".to_vec();
        path.resize(1 + kortty_ssh::MAX_EXEC_COMMAND_BYTES - overhead, b'a');
        assert_eq!(
            command(&[path.clone()]).unwrap().len(),
            kortty_ssh::MAX_EXEC_COMMAND_BYTES
        );
        *path.last_mut().unwrap() = b'\'';
        assert!(command(&[path]).is_err());
        assert!(command(&[vec![b'/'; kortty_ssh::MAX_EXEC_COMMAND_BYTES + 1]]).is_err());
    }
}
