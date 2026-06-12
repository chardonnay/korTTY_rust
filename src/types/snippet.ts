export type SnippetDiagramType = "PlantUml";

/** LINE, UNDERSCORE or BLOCK. */
export type SnippetEditorCursorStyle = "LINE" | "UNDERSCORE" | "BLOCK";

export interface SnippetHistoryEntry {
  content: string;
  timestamp: number;
}

export interface SnippetCodeReference {
  label: string;
  startLine: number;
  endLine: number;
}

export interface SnippetDiagram {
  id: string;
  name: string;
  diagramType: SnippetDiagramType;
  source: string;
  renderedPath?: string;
  contentHash?: string;
  title?: string;
  customInstructions?: string;
  codeReferences?: SnippetCodeReference[];
  createdAt?: number;
  updatedAt?: number;
}

export interface SnippetEditorProfile {
  id: string;
  name: string;
  language?: string;
  builtIn?: boolean;
  formatterCommand?: string;
  formatterArgs: string[];
  tabSize: number;
  insertSpaces: boolean;
  foregroundColor?: string;
  backgroundColor?: string;
  cursorStyle?: SnippetEditorCursorStyle;
  cursorColor?: string;
  commentColor?: string;
  stringColor?: string;
  numberColor?: string;
  booleanColor?: string;
  keyColor?: string;
  keywordColor?: string;
  sectionColor?: string;
  variableColor?: string;
  braceColor?: string;
}
