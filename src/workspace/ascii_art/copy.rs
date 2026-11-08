use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! options {
    ($name:ident, $($variant:ident => $label:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            #[default]
            None,
            $($variant),+
        }

        impl $name {
            pub(super) const ALL: &'static [Self] = &[Self::None, $(Self::$variant),+];
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(match self {
                    Self::None => "None",
                    $(Self::$variant => $label),+
                })
            }
        }
    };
}

options! {
    Comment,
    Hash => "# · Shell, Python, Ruby",
    Slash => "// · Java, C, Rust, JavaScript",
    Dash => "-- · SQL, Lua, Haskell",
    Semicolon => "; · Lisp, Clojure, INI",
    Percent => "% · LaTeX, MATLAB, Erlang",
    Quote => "' · VB, VBA",
    Rem => "REM · Batch",
    CBlock => "/* … */ · C-style",
    XmlBlock => "<!-- … --> · XML, HTML",
    Json => "[ … ] · JSON strings",
}

options! {
    Output,
    Heredoc => "Bash / sh heredoc",
    Bash => "Bash echo",
    Sh => "POSIX sh printf",
    Perl => "Perl",
    Python => "Python",
    Ruby => "Ruby",
    Php => "PHP",
    JavaScript => "JavaScript",
    Java => "Java",
    C => "C",
    CSharp => "C#",
    Go => "Go",
    Rust => "Rust",
    Kotlin => "Kotlin",
    Swift => "Swift",
    Lua => "Lua",
    PowerShell => "PowerShell",
    Batch => "Batch",
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct Format {
    pub(super) comment: Comment,
    pub(super) output: Output,
    pub(super) gap: u8,
}

impl Default for Format {
    fn default() -> Self {
        Self {
            comment: Comment::None,
            output: Output::None,
            gap: 1,
        }
    }
}

impl Format {
    pub(super) fn comment(&mut self, style: Comment) {
        self.comment = style;
        if style != Comment::None {
            self.output = Output::None;
        }
    }

    pub(super) fn output(&mut self, style: Output) {
        self.output = style;
        if style != Output::None {
            self.comment = Comment::None;
        }
    }

    pub(super) fn format(self, art: &str) -> String {
        if art.trim().is_empty() {
            return String::new();
        }
        let art = art.trim_end_matches('\n');
        let indent = " ".repeat(usize::from(self.gap.min(20)));
        if self.output == Output::Heredoc {
            let prefix = "KORTTY_ASCII_ART";
            let mut delimiter = prefix.to_owned();
            // Only a line with no leading whitespace can terminate a non-stripping heredoc.
            if indent.is_empty() {
                let mut count = 1;
                while art.lines().any(|line| line == delimiter) {
                    delimiter = format!("{prefix}_{count}");
                    count += 1;
                }
            }
            let body = art
                .lines()
                .map(|line| indented(&indent, line))
                .collect::<Vec<_>>()
                .join("\n");
            return format!("cat <<'{delimiter}'\n{body}\n{delimiter}");
        }
        if self.output != Output::None {
            return art
                .lines()
                .map(|line| self.output.statement(&indented(&indent, line)))
                .collect::<Vec<_>>()
                .join("\n");
        }
        self.comment.wrap(art, &indent)
    }

    pub(super) fn sample(self, art: &str) -> String {
        let first = art
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(" /\\_/\\");
        let sample = self.format(first);
        let skip_wrapper = self.output == Output::Heredoc
            || matches!(
                self.comment,
                Comment::CBlock | Comment::XmlBlock | Comment::Json
            );
        sample
            .lines()
            .nth(usize::from(skip_wrapper))
            .unwrap_or_default()
            .chars()
            .take(96)
            .collect()
    }
}

impl Comment {
    fn wrap(self, art: &str, indent: &str) -> String {
        use Comment::*;
        let marker = match self {
            None => return art.to_owned(),
            Hash => "#",
            Slash => "//",
            Dash => "--",
            Semicolon => ";",
            Percent => "%",
            Quote => "'",
            Rem => "REM",
            CBlock => {
                let lines = art
                    .lines()
                    .map(|line| format!(" *{}", indented(indent, line)).replace("*/", "+/"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return format!("/*\n{lines}\n */");
            }
            XmlBlock => {
                let lines = art
                    .lines()
                    .map(|line| {
                        let mut safe = String::new();
                        for character in indented(indent, line).chars() {
                            safe.push(if character == '-' && safe.ends_with('-') {
                                '='
                            } else {
                                character
                            });
                        }
                        safe
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return format!("<!--\n{lines}\n-->");
            }
            Json => {
                let lines = art
                    .lines()
                    .map(|line| format!("{indent}{}", serde_json::Value::String(line.to_owned())))
                    .collect::<Vec<_>>()
                    .join(",\n");
                return format!("[\n{lines}\n]");
            }
        };
        let separator = if indent.is_empty() && matches!(self, Rem | Dash) {
            " "
        } else {
            indent
        };
        let mut lines = art
            .lines()
            .map(|line| {
                let mut line = format!("{marker}{}", indented(separator, line));
                if self == Rem && line.ends_with('^') {
                    line.push(' ');
                }
                line
            })
            .collect::<Vec<_>>();
        if self == Slash
            && lines
                .last()
                .is_some_and(|line| line.trim_end().ends_with('\\'))
        {
            lines.push(marker.to_owned());
        }
        lines.join("\n")
    }
}

impl Output {
    fn statement(self, line: &str) -> String {
        use Output::*;
        let double = || line.replace('\\', "\\\\").replace('"', "\\\"");
        let single = || line.replace('\\', "\\\\").replace('\'', "\\'");
        let shell = || line.replace('\'', "'\\''");
        match self {
            None | Heredoc => line.to_owned(),
            Bash if !matches!(line, "-n" | "-e" | "-E") => format!("echo '{}'", shell()),
            Bash | Sh => format!("printf '%s\\n' '{}'", shell()),
            Perl => format!("print '{}', \"\\n\";", single()),
            Python | Swift | Lua => format!("print(\"{}\")", double()),
            Ruby => format!("puts '{}'", single()),
            Php => format!("echo '{}', PHP_EOL;", single()),
            JavaScript => format!("console.log(\"{}\");", double()),
            Java => format!("System.out.println(\"{}\");", double()),
            C => {
                let mut safe = String::new();
                let mut previous = '\0';
                for character in double().chars() {
                    if previous == '?' && character == '?' {
                        safe.push('\\');
                    }
                    safe.push(character);
                    previous = character;
                }
                format!("puts(\"{safe}\");")
            }
            CSharp => format!("Console.WriteLine(\"{}\");", double()),
            Go => format!("fmt.Println(\"{}\")", double()),
            Rust => format!(
                "println!(\"{}\");",
                double().replace('{', "{{").replace('}', "}}")
            ),
            Kotlin => format!("println(\"{}\")", double().replace('$', "\\$")),
            PowerShell => format!("Write-Output '{}'", line.replace('\'', "''")),
            Batch => {
                let mut result = String::from("echo(");
                for character in line.chars() {
                    if "^&|<>()\"".contains(character) {
                        result.push('^');
                    } else if character == '%' {
                        result.push('%');
                    }
                    result.push(character);
                }
                result
            }
        }
    }
}

fn indented(indent: &str, line: &str) -> String {
    if line.trim().is_empty() {
        String::new()
    } else {
        format!("{indent}{line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_art_copy_protects_script_and_comment_boundaries() {
        let mut format = Format {
            gap: 0,
            ..Format::default()
        };
        format.output(Output::Heredoc);
        let input = "KORTTY_ASCII_ART\nKORTTY_ASCII_ART_1\n$(echo X)";
        assert_eq!(
            format.format(input),
            format!("cat <<'KORTTY_ASCII_ART_2'\n{input}\nKORTTY_ASCII_ART_2")
        );

        format.comment(Comment::CBlock);
        assert_eq!(format.format("/a*/"), "/*\n +/a+/\n */");
        format.comment(Comment::XmlBlock);
        assert_eq!(format.format("----"), "<!--\n-=-=\n-->");
        format.comment(Comment::Slash);
        assert_eq!(format.format("end\\ "), "//end\\ \n//");

        format.output(Output::Rust);
        assert_eq!(format.format("{x}"), "println!(\"{{x}}\");");
        format.comment(Comment::Json);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&format.format("\"a\"\n\\b")).unwrap(),
            ["\"a\"", "\\b"]
        );
    }
}
