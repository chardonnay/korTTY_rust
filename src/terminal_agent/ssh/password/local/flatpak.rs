use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use kortty_pty::{LaunchOptions, Pty, flatpak};

const CHECK_DIRECTORY: &str = r#"
identity=$(/usr/bin/stat -L -c '%d:%i' -- .) || exit 97
physical=$(/bin/pwd -P) || exit 97
[ "$identity" = "$1" ] && [ "$physical" = "$2" ] || exit 97
shift 2
exec "$@"
"#;

pub(super) fn spawn(path: &Path, arguments: Vec<String>, directory: &File) -> io::Result<Pty> {
    if !flatpak::running() {
        return Err(io::Error::other("The Flatpak host shell is unavailable"));
    }
    Pty::spawn_with_options(
        80,
        24,
        "xterm-256color",
        &LaunchOptions {
            command: Some(checked_arguments(path, arguments, directory)?),
            working_directory: Some(path.into()),
            ..LaunchOptions::default()
        },
    )
}

fn checked_arguments(
    path: &Path,
    arguments: Vec<String>,
    directory: &File,
) -> io::Result<Vec<String>> {
    // The caller keeps the reviewed directory open for the lifetime of the
    // PTY. The host checks its actual working directory before sudo can prompt.
    let metadata = directory.metadata()?;
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::other("The reviewed Flatpak host path is unavailable"))?;
    let mut command = vec![
        "/bin/sh".into(),
        "-c".into(),
        CHECK_DIRECTORY.into(),
        "kortty-host-sudo".into(),
        format!("{}:{}", metadata.dev(), metadata.ino()),
        path.into(),
    ];
    command.extend(arguments);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn host_sudo_launch_checks_the_physical_path_and_open_directory_identity() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let reviewed = root.join("reviewed 'directory'");
        let moved = root.join("moved");
        std::fs::create_dir(&reviewed).unwrap();
        let pinned = File::open(&reviewed).unwrap();
        let guarded = checked_arguments(
            &reviewed,
            vec!["/usr/bin/printf".into(), "fixture challenge".into()],
            &pinned,
        )
        .unwrap();
        let run = |cwd: &Path| {
            Command::new(&guarded[0])
                .args(&guarded[1..])
                .current_dir(cwd)
                .output()
                .unwrap()
        };
        let output = run(&reviewed);
        assert!(output.status.success());
        assert_eq!(output.stdout, b"fixture challenge");

        std::fs::rename(&reviewed, &moved).unwrap();
        std::fs::create_dir(&reviewed).unwrap();
        for unapproved in [&moved, &reviewed] {
            let output = run(unapproved);
            assert_eq!(output.status.code(), Some(97));
            assert!(output.stdout.is_empty());
        }
    }
}
