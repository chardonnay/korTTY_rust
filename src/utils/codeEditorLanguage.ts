import { StreamLanguage } from "@codemirror/language";
import { perl } from "@codemirror/legacy-modes/mode/perl";
import { ruby } from "@codemirror/legacy-modes/mode/ruby";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { sql } from "@codemirror/lang-sql";
import { json } from "@codemirror/lang-json";
import { xml } from "@codemirror/lang-xml";
import { yaml } from "@codemirror/lang-yaml";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { markdown } from "@codemirror/lang-markdown";
import { php } from "@codemirror/lang-php";
import { rust } from "@codemirror/lang-rust";
import { java } from "@codemirror/lang-java";
import { cpp } from "@codemirror/lang-cpp";
import type { Extension } from "@codemirror/state";

type LanguageDefinition = {
  aliases: string[];
  label: string;
  extension: () => Extension;
};

const LANGUAGE_DEFINITIONS: LanguageDefinition[] = [
  {
    aliases: ["bash", "shell", "sh", "zsh", "fish"],
    label: "Bash / Shell",
    extension: () => StreamLanguage.define(shell),
  },
  { aliases: ["c", "h"], label: "C", extension: cpp },
  { aliases: ["cpp", "cxx", "cc", "hpp", "hxx"], label: "C++", extension: cpp },
  { aliases: ["css"], label: "CSS", extension: css },
  { aliases: ["html"], label: "HTML", extension: html },
  { aliases: ["java"], label: "Java", extension: java },
  { aliases: ["javascript", "js", "jsx"], label: "JavaScript", extension: javascript },
  { aliases: ["json"], label: "JSON", extension: json },
  { aliases: ["markdown", "md"], label: "Markdown", extension: markdown },
  { aliases: ["php"], label: "PHP", extension: php },
  {
    aliases: ["perl", "pl", "pm"],
    label: "Perl",
    extension: () => StreamLanguage.define(perl),
  },
  { aliases: ["python", "py"], label: "Python", extension: python },
  {
    aliases: ["ruby", "rb", "gemspec"],
    label: "Ruby",
    extension: () => StreamLanguage.define(ruby),
  },
  { aliases: ["rust", "rs"], label: "Rust", extension: rust },
  { aliases: ["sql"], label: "SQL", extension: sql },
  {
    aliases: ["typescript", "ts", "tsx"],
    label: "TypeScript",
    extension: () => javascript({ typescript: true }),
  },
  { aliases: ["xml"], label: "XML", extension: xml },
  { aliases: ["yaml", "yml"], label: "YAML", extension: yaml },
];

function normalizeLanguageTag(language?: string | null): string {
  return (language || "").trim().toLowerCase();
}

export function getCodeEditorExtensions(language?: string | null): Extension[] {
  const normalized = normalizeLanguageTag(language);
  if (!normalized) {
    return [];
  }

  const definition = LANGUAGE_DEFINITIONS.find((entry) =>
    entry.aliases.includes(normalized),
  );
  if (!definition) {
    return [];
  }

  return [definition.extension()];
}

export function getCodeLanguageLabel(language?: string | null): string {
  const normalized = normalizeLanguageTag(language);
  if (!normalized) {
    return "Plain Text";
  }

  const definition = LANGUAGE_DEFINITIONS.find((entry) =>
    entry.aliases.includes(normalized),
  );
  return definition?.label ?? normalized;
}
