// Maps KorTTY snippet/file language names to Monaco language ids.
// Port of de.kortty.ui.MonacoLanguageSupport together with the language
// normalization from de.kortty.core.SnippetLanguageSupport.

export function normalizeSnippetLanguage(language?: string | null): string {
  const normalized = (language ?? "").trim().toLowerCase();
  if (!normalized) {
    return "plain";
  }
  switch (normalized) {
    case "sh":
    case "shell":
    case "zsh":
    case "bash":
      return "bash";
    case "py":
    case "python":
    case "python3":
      return "python";
    case "pl":
    case "perl":
      return "perl";
    case "rb":
    case "ruby":
      return "ruby";
    case "js":
    case "javascript":
    case "node":
    case "nodejs":
      return "javascript";
    case "ps":
    case "ps1":
    case "pwsh":
    case "powershell":
      return "powershell";
    case "groovy":
      return "groovy";
    case "java":
      return "java";
    case "json":
      return "json";
    case "yaml":
    case "yml":
      return "yaml";
    case "xml":
      return "xml";
    case "markdown":
    case "md":
      return "markdown";
    case "asciidoctor":
    case "asciidoc":
    case "adoc":
      return "asciidoctor";
    case "sql":
      return "sql";
    case "dockerfile":
      return "dockerfile";
    case "properties":
    case "ini":
      return "properties";
    case "html":
      return "html";
    case "plain":
    case "text":
    case "txt":
      return "plain";
    default:
      return normalized;
  }
}

export function toMonacoLanguage(language?: string | null): string {
  const normalized = normalizeSnippetLanguage(language);
  switch (normalized) {
    case "plain":
    case "text":
    case "plaintext":
      return "plaintext";
    case "bash":
    case "shell":
    case "sh":
    case "zsh":
    case "fish":
      return "shell";
    case "yml":
    case "ansible_yaml":
      return "yaml";
    case "properties":
    case "cfg":
    case "conf":
      return "ini";
    case "terraform":
      return "hcl";
    case "groovy":
      return "java";
    case "javascript":
    case "js":
    case "jsx":
      return "javascript";
    case "typescript":
    case "ts":
    case "tsx":
      return "typescript";
    case "powershell":
    case "ps1":
      return "powershell";
    case "dockerfile":
      return "dockerfile";
    case "cfengine3":
      return "cfengine3";
    case "jinja2":
      return "jinja2";
    case "puppet":
      return "puppet";
    case "toml":
      return "toml";
    case "c":
    case "h":
      return "c";
    case "cpp":
    case "cxx":
    case "cc":
    case "hpp":
    case "hxx":
      return "cpp";
    case "rs":
      return "rust";
    case "pm":
      return "perl";
    case "gemspec":
      return "ruby";
    case "php":
      return "php";
    case "python":
    case "perl":
    case "ruby":
    case "java":
    case "css":
    case "go":
    case "rust":
    case "sql":
    case "xml":
    case "json":
    case "yaml":
    case "html":
    case "markdown":
    case "ini":
    case "hcl":
      return normalized;
    default:
      return "plaintext";
  }
}
