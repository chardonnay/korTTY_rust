import * as monaco from "monaco-editor";
import { loader } from "@monaco-editor/react";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import JsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import CssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import HtmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import TsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

// Bundle Monaco with Vite instead of loading it from the CDN loader of
// @monaco-editor/react. Workers are resolved through Vite "?worker" imports
// so the app keeps working fully offline.
self.MonacoEnvironment = {
  getWorker(_moduleId: string, label: string): Worker {
    switch (label) {
      case "json":
        return new JsonWorker();
      case "css":
      case "less":
      case "scss":
        return new CssWorker();
      case "html":
      case "handlebars":
      case "razor":
        return new HtmlWorker();
      case "typescript":
      case "javascript":
        return new TsWorker();
      default:
        return new EditorWorker();
    }
  },
};

loader.config({ monaco });

type MonarchLanguage = Parameters<typeof monaco.languages.setMonarchTokensProvider>[1];

function registerMonarch(id: string, language: MonarchLanguage): void {
  if (!monaco.languages.getLanguages().some((entry) => entry.id === id)) {
    monaco.languages.register({ id });
  }
  monaco.languages.setMonarchTokensProvider(id, language);
}

// Extra languages that Monaco does not ship out of the box, ported from the
// Java bundle (src/monaco/host.js registerExtraLanguages).
function registerExtraLanguages(): void {
  registerMonarch("toml", {
    tokenizer: {
      root: [
        [/#.*$/, "comment"],
        [/\[[^\]]+\]/, "keyword"],
        [/[A-Za-z0-9_.-]+(?=\s*=)/, "key"],
        [/"([^"\\]|\\.)*"|'([^'\\]|\\.)*'/, "string"],
        [/\b(true|false)\b/, "keyword"],
        [/\b-?\d+(\.\d+)?\b/, "number"],
      ],
    },
  });
  registerMonarch("jinja2", {
    tokenizer: {
      root: [
        [/\{#[\s\S]*?#\}/, "comment"],
        [/\{\{[\s\S]*?\}\}/, "variable"],
        [/\{%[\s\S]*?%\}/, "keyword"],
        [/"([^"\\]|\\.)*"|'([^'\\]|\\.)*'/, "string"],
      ],
    },
  });
  registerMonarch("puppet", {
    tokenizer: {
      root: [
        [/#.*$/, "comment"],
        [/"([^"\\]|\\.)*"|'([^'\\]|\\.)*'/, "string"],
        [/\b(class|define|node|include|require|contain|if|elsif|else|unless|case|true|false|undef|default)\b/, "keyword"],
        [/\$[a-zA-Z_][a-zA-Z0-9_:]*/, "variable"],
        [/=>|->|~>|\+>|<\||\|>/, "operator"],
      ],
    },
  });
  registerMonarch("cfengine3", {
    tokenizer: {
      root: [
        [/#.*$/, "comment"],
        [/"([^"\\]|\\.)*"|'([^'\\]|\\.)*'/, "string"],
        [/\b(bundle|body|promise|agent|common|server|classes|commands|files|methods|packages|processes|reports|vars|defaults)\b/, "keyword"],
        [/\$\([^)]+\)|\$\{[^}]+\}|@\([^)]+\)|@\{[^}]+\}/, "variable"],
        [/[a-zA-Z_]+:/, "keyword"],
        [/=>|->/, "operator"],
      ],
    },
  });
}

registerExtraLanguages();

export { monaco };
