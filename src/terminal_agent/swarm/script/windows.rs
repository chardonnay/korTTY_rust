//! Windows command payloads keep reviewed script and arguments out of shell syntax.

use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use zeroize::Zeroizing;

use super::valid_command;

pub(super) struct Command {
    pub(super) wire: String,
    pub(super) preview: String,
}

pub(super) fn powershell(source: &str, arguments: &[String]) -> Result<Command, &'static str> {
    if !valid_command(source)
        || arguments.len() > 64
        || arguments
            .iter()
            .any(|argument| !valid_command(argument) || argument.contains(['\n', '\r']))
    {
        return Err("The PowerShell script or its literal arguments cannot be reviewed safely.");
    }
    let mut wire = String::from("$__korttyArgs = @(\n");
    for argument in arguments {
        let encoded = Zeroizing::new(STANDARD.encode(argument));
        let _ = writeln!(
            wire,
            "[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}'))",
            encoded = *encoded
        );
    }
    let encoded = Zeroizing::new(STANDARD.encode(source));
    let _ = write!(
        wire,
        concat!(
            ")\n& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')))) @__korttyArgs\n",
            "$__korttySucceeded = $?\n",
            "if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}\n",
            "if (!$__korttySucceeded) {{ exit 1 }}"
        ),
        *encoded
    );
    if !valid_command(&wire) {
        return Err("The encoded PowerShell script and arguments exceed the command limit.");
    }
    let args = serde_json::to_string_pretty(arguments)
        .map_err(|_| "Could not render the script arguments.")?;
    Ok(Command {
        wire,
        preview: format!("PowerShell script\n{source}\n\nLiteral positional arguments\n{args}"),
    })
}

pub(super) fn cmd(source: &str, arguments: &[String]) -> Result<Command, &'static str> {
    let command = source.strip_suffix('\n').unwrap_or(source);
    if !arguments.is_empty() || command.contains(['\r', '\n']) || !valid_command(command) {
        return Err(
            "Command Prompt accepts a single reviewed command line without script arguments.",
        );
    }
    Ok(Command {
        wire: command.into(),
        preview: command.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_windows_powershell_arguments_and_source_are_encoded_as_literal_data() {
        let source = "param([string]$Text)\n[Console]::WriteLine($Text)\nexit 23";
        let argument = "-switch $(throw 'expanded') 火";
        let command = powershell(source, &[argument.into()]).unwrap();
        assert!(command.preview.contains(source));
        assert!(command.preview.contains(argument));
        assert!(!command.wire.contains(argument));
        assert!(!command.wire.contains(source));
        assert!(command.wire.contains(&STANDARD.encode(argument)));
        assert!(command.wire.contains(&STANDARD.encode(source)));
        assert!(powershell(&"a".repeat(4096), &[]).is_err());
    }

    #[test]
    fn swarm_windows_cmd_does_not_interpolate_batch_arguments_or_multiline_source() {
        let command = cmd("echo reviewed & exit /b 7\n", &[]).unwrap();
        assert_eq!(command.wire, "echo reviewed & exit /b 7");
        assert_eq!(command.preview, command.wire);
        assert!(cmd("echo %1", &["& exit /b 77".into()]).is_err());
        assert!(cmd("echo first\necho second", &[]).is_err());
    }
}
