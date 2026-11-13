use super::*;
use crate::snippets::{Input, Library};
use crate::terminal_agent::swarm::script::Prepared;

#[test]
#[ignore = "requires KORTTY_SSH_TEST_EXE pointing to the actual kortty executable"]
fn native_swarm_powershell_script_preserves_reviewed_source_and_literal_arguments() {
    let executable = std::env::var_os("KORTTY_SSH_TEST_EXE")
        .map(PathBuf::from)
        .expect("set KORTTY_SSH_TEST_EXE to the built desktop executable")
        .canonicalize()
        .unwrap();
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let source = concat!(
        "param([string]$First, [string]$Second)\n",
        "[Console]::Out.Write($First)\n",
        "[Console]::Error.Write($Second)\n",
        "exit 29"
    );
    let id = library
        .save(
            None,
            Input {
                name: "Windows workflow".into(),
                language: "powershell".into(),
                category: String::new(),
                content: source.into(),
            },
        )
        .unwrap();
    let first = "-looks-like-a-switch";
    let second = "literal $(throw 'expanded') 東京";
    let review = Prepared::capture(&library, id, &format!("{first}\n{second}")).unwrap();
    assert!(review.preview().contains(source));
    assert!(review.preview().contains(second));
    let shell =
        Shell::PowerShell(kortty_pty::shell_program(kortty_pty::LocalShell::PowerShell).unwrap());
    let result = run_executable(
        &shell,
        &profile.0,
        &review.command,
        &Cancel::default(),
        &executable,
    )
    .unwrap();
    assert_eq!(result.status, Status::Exited(29));
    assert_eq!(&*result.stdout.text(), first);
    assert_eq!(&*result.stderr.text(), second);
}
