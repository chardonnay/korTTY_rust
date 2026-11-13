use std::sync::LazyLock;

use regex::Regex;

static BARE_PROMPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*(?:\([^)]{1,40}\)\s*)?(?:\[?[a-z_][a-z0-9_.-]{0,31}@[A-Za-z0-9_.-]+",
        r"(?:[: ][^\s\]$#%]*)?\]?\s?[$#%]|(?:ba|z|k|da)?sh-[0-9.]+[$#]|[$#%>❯])\s*$",
    ))
    .expect("static bare shell prompt")
});

static HELP_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:^|\s)(?:usage|aufruf|verwendung|benutzung|utilisation|uso|gebruik|upotreba|synopsis)\s*:",
        r"|^\s*(?:options|optionen|flags|commands|befehle|available commands|global options)\s*:?\s*$",
        r"|^\s*(?:synopsis|übersicht)\s*$",
    ))
    .expect("static help heading")
});

static NOT_FOUND: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        concat!(
            r"(?i)^\s*-?(?:(?:ba|z|k|da)?sh|sudo):\s+(?:(?:line|Zeile|ligne|línea|riga|linha|regel|redak)\s+)?",
            r"(?:\d+:\s+)?(\S+):\s+(?:command not found|Befehl nicht gefunden|Kommando nicht gefunden|",
            r"commande introuvable|orden no encontrada|comando no encontrado|comando non trovato|",
            r"comando não encontrado|opdracht niet gevonden|naredba nije pronađena|not found|",
            r"No such file or directory|Datei oder Verzeichnis nicht gefunden|Aucun fichier ou dossier de ce type|",
            r"No existe el archivo o el directorio|File o directory non esistente|",
            r"Arquivo ou diretório inexistente|Bestand of map bestaat niet)[.…]*\s*$",
        ),
        r"(?i)^\s*zsh:\s+(?:command not found|no such file or directory|Befehl nicht gefunden|Datei oder Verzeichnis nicht gefunden):\s+(\S+)\s*$",
        r"(?i)^\s*fish:\s+Unknown command:?\s+(\S+)\s*$",
        r"(?i)^\s*Command '([^']+)' not found.*$",
        r"^\s*Befehl »([^«]+)« nicht gefunden.*$",
    ]
    .map(|pattern| Regex::new(pattern).expect("static shell error"))
});

pub(in crate::session_journal) fn idle(line: &str) -> bool {
    line.trim().is_empty() || BARE_PROMPT.is_match(line)
}

pub(in crate::session_journal) fn help_heading(line: &str) -> bool {
    HELP_HEADING.is_match(line)
}

pub(in crate::session_journal) fn help_only(command: &str, heading: bool) -> bool {
    let Some(command) = simple(command) else {
        return false;
    };
    let mut words = command.split_whitespace().peekable();
    while words
        .peek()
        .is_some_and(|word| matches!(*word, "sudo" | "command" | "builtin"))
    {
        words.next();
    }
    let Some(program) = words.next() else {
        return false;
    };
    let program = program
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        program.as_str(),
        "man" | "info" | "tldr" | "help" | "whatis" | "apropos"
    ) {
        return true;
    }
    let args: Vec<_> = words.collect();
    let Some((last, between)) = args.split_last() else {
        return false;
    };
    if between.iter().any(|word| word.starts_with('-')) {
        return false;
    }
    if args[0] == "help" {
        return heading
            || matches!(
                program.as_str(),
                "git"
                    | "go"
                    | "docker"
                    | "podman"
                    | "cargo"
                    | "rustup"
                    | "npm"
                    | "pnpm"
                    | "yarn"
                    | "pip"
                    | "pip3"
                    | "kubectl"
                    | "helm"
                    | "gh"
                    | "terraform"
                    | "dotnet"
                    | "dnf"
                    | "yum"
                    | "apt"
                    | "zypper"
                    | "flatpak"
                    | "snap"
                    | "brew"
                    | "nmcli"
                    | "virsh"
                    | "openssl"
                    | "make"
            );
    }
    match last.to_ascii_lowercase().as_str() {
        "--help" | "-help" | "--usage" => true,
        "-h" | "-?" | "/?" => heading,
        _ => false,
    }
}

pub(super) fn failed_start(command: &str, output: &str) -> bool {
    let Some(command) = simple(command) else {
        return false;
    };
    let executable = command
        .split_whitespace()
        .find(|word| *word != "sudo" && !word.starts_with('-') && !word.contains('='));
    not_found_command(output).is_some_and(|name| Some(name) == executable)
}

pub(in crate::session_journal) fn not_found_command(output: &str) -> Option<&str> {
    NOT_FOUND.iter().find_map(|matcher| {
        matcher
            .captures(output)?
            .get(1)
            .map(|matched| matched.as_str())
    })
}

fn simple(command: &str) -> Option<&str> {
    (!command.contains(['|', ';', '&', '<', '>', '`', '\n']) && !command.contains("$("))
        .then_some(command)
}
