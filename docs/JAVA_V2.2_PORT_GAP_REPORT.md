
## Bereich: sftp (5 Lücken)

### [PARTIAL] [medium] Fix SFTP temporary SSH key reuse - unified connection copy logic
- Java: commit a793665: SftpConnectionSupport.connectionForSftp(), MainWindow, SFTPManagerDialog, SFTPManagerTab
- Lücke: Rust backend has temporary_key_content field in SessionSettings (src-tauri/src/ssh/session.rs:794-807) and supports decoding temporary keys inline with fallback to TEMPORARY: prefix handling. However, no standalone helper function equivalent to Java's SftpConnectionSupport.connectionForSftp() exists. Frontend (React) passes temporary key content directly to SSH session; the refactoring that centralizes connection-copy logic for SFTP has not been ported.
- Verifikation: Rust backend DOES support temporary key content: src-tauri/src/model/connection.rs line 32 has 'temporary_key_content: Option<String>' field, and src-tauri/src/ssh/session.rs lines 794-807 in resolve_private_key() handles temporary key decoding with fallback to TEMPORARY: prefix (lines 794-825). However, there is NO standalone helper function equivalent to Java's SftpConnectionSupport.connectionForSftp() that creates a copy of connections for SFTP with temporary key substitution. The Rust implementation handles temporary keys inline during connection setup rather than through a dedicated connection-copy abstraction.

### [MISSING] [high] SFTP subsystem failure diagnostics and fallback handling
- Java: commits 46d6ac0, 7abaa39: SFTPSession.sftpSubsystemFailureMessage(), SFTPSession.isSftpSubsystemNegotiationFailure(), detection of EOFException, 'Channel closing', 'closed before version negotiated', 'subsystem request failed' (case-insensitive)
- Lücke: Java v2.2 catches IOException/RuntimeException on SftpClientFactory.createSftpClient(), generates localized German-language diagnostics (in Java: 'SFTP-Subsystem wurde nach erfolgreicher SSH-Authentifizierung vom Server abgelehnt oder geschlossen...'), and distinguishes negotiation failures from generic startup failures. Rust backend uses SSH exec_command() for file operations (base64-encoded uploads/downloads) and does not establish SFTP subsystems at all. No error detection or user-facing diagnostics for SFTP protocol failures exist.
- Verifikation: Rust backend uses exec_command() with base64 encoding for all remote file operations (src-tauri/src/commands/sftp_commands.rs lines 195-296). It does NOT establish SFTP subsystems at all, meaning the entire diagnostic feature class is architecturally absent. Java v2.2 implements SFTPSession.sftpSubsystemFailureMessage() and isSftpSubsystemNegotiationFailure() (src/main/java/de/kortty/core/SFTPSession.java lines 200-227) to detect and localize protocol failures ('SFTP-Subsystem wurde...', 'Channel closing', 'closed before version negotiated', 'subsystem request failed'). Rust backend has no error detection, no subsystem negotiation logic, and no user-facing diagnostics for SFTP protocol failures because the architecture avoids SFTP subsystems entirely.

### [PARTIAL] [high] SFTP Snippet Editor - file overwrite and save-as action handling
- Java: commit 70e95b9: SFTPManagerTab.overwriteRemoteSnippetFile(), saveRemoteSnippetFileAs(), overwriteLocalSnippetFile(), saveLocalSnippetFileAs() with dialog/file-chooser integration
- Lücke: Rust backend has write_local_text_file and write_remote_text_file commands (src-tauri/src/commands/sftp_commands.rs:164-296) for simple overwrite. Java supports: (1) overwrite file at source path, (2) 'Save As' dialog for local files (FileChooser), (3) 'Save As' for remote with input dialog + resolveSiblingRemoteFilePath() validation, (4) save as new snippet. Rust frontend/backend lacks: Save-As dialog UI flow, sibling file path resolution validation (validateRemoteSiblingFileName, appendRemoteName), and integration into SnippetManager.
- Verifikation: Rust backend HAS basic overwrite via write_local_text_file and write_remote_text_file (src-tauri/src/commands/sftp_commands.rs lines 164-296). Frontend passes 'Save' callback to SnippetManager which overwrites at source path (src/components/dialogs/SnippetManager.tsx). MISSING: (1) Save-As dialog UI flow - there is no dialog to choose a new filename for local or remote files, (2) no input dialog for remote save-as path, (3) no sibling file path resolution validation (validateRemoteSiblingFileName, appendRemoteName, resolveSiblingRemoteFilePath), (4) no integration into SnippetManager for Save-As vs Overwrite branching. The edit flow (src/components/sftp/SFTPManager.tsx line 624 handleEditFile) creates a file draft and passes it to SnippetManager, but no Save-As UI exists. Java supports all four actions: overwrite file at source path, Save-As local with FileChooser dialog, Save-As remote with input dialog and validation, and save as new snippet.

### [MISSING] [high] Remote file path sibling resolution for 'Save As' action
- Java: commit 70e95b9: SftpFileTransferService.resolveSiblingRemoteFilePath(), validateRemoteSiblingFileName()
- Lücke: Java validates remote file names (no '.' or '..', no '/', no backslash) and resolves siblings by extracting parent directory and appending sanitized new name. Rust backend has no equivalent helper. SnippetEditor 'Save As' for remote files cannot be properly implemented without this validation.
- Verifikation: Rust backend has no equivalent helper for validating or resolving sibling remote file paths. Java provides resolveSiblingRemoteFilePath() and validateRemoteSiblingFileName() (src/main/java/de/kortty/core/SftpFileTransferService.java lines 169-334) which: (1) validates file names (rejects '.', '..', '/', '\'), (2) extracts parent directory from original path, (3) appends sanitized new name. Rust backend has no such validation layer in either frontend or backend. The SFTP file edit flow (src/components/sftp/SFTPManager.tsx line 624) reads files but has no Save-As capability that would require sibling resolution.

### [MISSING] [medium] Remote text file selection support - validation and normalization
- Java: commit 70e95b9 (new file): RemoteTextFileSelectionSupport with normalizeSelectedFileName(), resolveRemoteFilePath(), decodeUtf8TextFile(), BinaryOrNonTextFileException
- Lücke: Java v2.2 introduces utility to validate terminal selections as remote file names: strips quotes, checks for null bytes/control chars, rejects '.' '..' paths, detects binary files via UTF-8 decoding strictness, and resolves working-directory-relative paths for SFTP editing. No equivalent exists in Rust backend or frontend.
- Verifikation: Rust backend has no equivalent to Java's RemoteTextFileSelectionSupport utility (src/main/java/de/kortty/core/RemoteTextFileSelectionSupport.java lines 1-153). Java v2.2 introduces normalizeSelectedFileName(), resolveRemoteFilePath(), decodeUtf8TextFile(), and BinaryOrNonTextFileException to validate terminal selections as remote file names: strips quotes, checks for null bytes/control chars, rejects '.' '..' paths, detects binary files via UTF-8 strictness, resolves working-directory-relative paths. Rust SFTP file edit (src/components/sftp/SFTPManager.tsx line 624 handleEditFile) directly reads files without: (1) terminal selection parsing/normalization, (2) quote stripping, (3) binary file detection via UTF-8 validation, (4) working-directory resolution. No SnippetEditor 'Open Remote File' feature exists to justify these utilities.

## Bereich: snippet-editor (24 Lücken)

### [PARTIAL] [high] Monaco-based editor (WebView-hosted)
- Java: fd94f81 (Rüste den Snippet-Editor auf Monaco um), src/main/java/de/kortty/ui/MonacoEditorPane.java (632 lines added), src/main/java/de/kortty/ui/MonacoLanguageSupport.java
- Rust-Stand: src/components/dialogs/SnippetManager.tsx uses CodeMirror; src/utils/codeEditorLanguage.ts provides language definitions
- Lücke: Java uses WebView-hosted Monaco with local HTML resources and bidirectional JavaScript/Java communication (text, selection, undo/redo, ruler state sync). Rust uses CodeMirror (@uiw/react-codemirror) with language extensions from @codemirror/lang-* packages. Both provide syntax highlighting and editing; Monaco offers richer features (diff viewer, theme integration). CodeMirror is a simpler alternative without Monaco's full feature set.
- Verifikation: Rust uses CodeMirror (@uiw/react-codemirror v4.25.4) with @codemirror/lang-* packages for syntax highlighting and editing. Both provide syntax highlighting; Monaco offers richer features like diff viewer, but CodeMirror is a simpler alternative that covers basic editing needs. The claim of 'partial' is correct - the editors differ architecturally but both provide code editing with language support.

### [MISSING] [high] Column ruler with live caret marker and line-length limiter (20-240 chars)
- Java: fd94f81, src/main/java/de/kortty/ui/SnippetColumnRuler.java (303 lines new), tracks caretColumn, limitColumn, supports context menu for format-to-limit and clear-limit actions
- Rust-Stand: SnippetManager.tsx has no ruler component; src/model/snippet.rs has no ruler-state fields
- Lücke: Java: Custom Canvas-based ruler above editor showing column positions, caret marker, and optional line-length limit marker. Context menu allows setting/clearing limit (20-240 range) and formatting code to that width. Rust: No ruler component exists. Snippet editor in SnippetManager has no visual ruler, caret-column display, or line-width marker UI.

### [MISSING] [high] Format code to ruler width (line-length aware formatting)
- Java: fd94f81, src/main/java/de/kortty/core/CodeFormatterService.java adds supportsLineWidth(), normalizeLineWidth() (20-240 range), commandLineWithLineWidth() for Prettier, Black, Perl::Tidy; SnippetColumnRuler context menu integrates formatting action
- Rust-Stand: src-tauri/src/snippet_tools.rs:format_snippet_code does not support maxLineLength; SnippetManager.tsx handleFormatSnippet() passes only {content, language}
- Lücke: Java: CodeFormatterService.format() accepts optional maxLineLength parameter. Prettier (--print-width), Black (--line-length), and Perl::Tidy (-l=) receive width arguments. SnippetColumnRuler.formatAtLimit() triggers formatting. Rust: format_snippet_code() in snippet_tools.rs does not accept line-width parameter; built-in formatters (JSON, XML) and external formatters lack width configuration.
- Verifikation: Rust's format_snippet_code() in snippet_tools.rs does not accept maxLineLength parameter. Built-in formatters (JSON, XML) and external formatter support (via formatter_command/args) lack line-width configuration. No support for Prettier --print-width, Black --line-length, or Perl::Tidy -l= parameters.

### [MISSING] [high] Monaco diff viewer (side-by-side read-only comparison)
- Java: fd94f81, src/main/java/de/kortty/ui/MonacoDiffPane.java (245 lines new), src/main/java/de/kortty/ui/SnippetDiffDialog.java (97 lines new), SnippetManagementDialog shows diff action for two selected snippets
- Rust-Stand: src/components/dialogs/SnippetManager.tsx has no diff UI; no diff commands in src-tauri/src/commands/snippet_commands.rs
- Lücke: Java: MonacoDiffPane wraps Monaco's diff-editor with WebView, displays two snippets side-by-side with syntax highlighting per language. SnippetDiffDialog opens a dialog showing left/right snippets with detected languages and comparison. Rust: No diff functionality; SnippetManager lacks diff action/dialog for comparing snippets.
- Verifikation: No diff viewer functionality in Rust. SnippetManager lacks diff action or SnippetDiffDialog. No MonacoDiffPane equivalent or side-by-side snippet comparison UI.

### [PARTIAL] [medium] Editor profiles (10 built-in IntelliJ-inspired + user-defined color/cursor profiles)
- Java: src/main/java/de/kortty/model/SnippetEditorProfile.java (new), src/main/java/de/kortty/core/SnippetEditorProfileSupport.java (new), src/main/java/de/kortty/ui/SnippetEditorProfileDialog.java (new), SnippetEditDialog integrates profile selection
- Rust-Stand: src-tauri/src/model/snippet.rs:SnippetEditorProfile missing color/cursor fields; SnippetManager.tsx has no profile selector UI
- Lücke: Java: SnippetEditorProfile stored in GlobalSettings with id, name, language, foregroundColor, backgroundColor, cursorColor, fontSize, fontFamily, cursorStyle. Built-in profiles (IntelliJ Light, Dark, Monokai, etc.) selectable in dialog. Rust: SnippetEditorProfile model exists with formatter_command/args, tab_size, insert_spaces, but no color/cursor fields. No UI to manage profiles; profile field in Snippet is not used in SnippetManager.
- Verifikation: SnippetEditorProfile model exists in Rust with id, name, language, formatter_command, formatter_args, tab_size, insert_spaces fields. But it lacks color fields (foregroundColor, backgroundColor, cursorColor), fontSize, fontFamily, and cursorStyle. No built-in profiles, no profile selection UI in SnippetManager, and profile field in Snippet is defined but never used in the editor.

### [MISSING] [medium] Snippet history with restore (list of content changes with timestamps)
- Java: e652da2 (Add snippet history), src/main/java/de/kortty/model/SnippetHistoryEntry.java (new), Snippet.history field, SnippetEditDialog history UI with restore actions
- Rust-Stand: src-tauri/src/model/snippet.rs:Snippet lacks history field; SnippetManager.tsx has no history UI
- Lücke: Java: Snippet.history is List<SnippetHistoryEntry> persisted in XML. Each entry has content string and epoch-millis timestamp. SnippetEditDialog shows history panel with entries sortable by timestamp, restore action reverts to prior version. Rust: No history field in Snippet model; no history tracking or UI in SnippetManager.
- Verifikation: Snippet model in Rust has no history field. No SnippetHistoryEntry equivalent, no timestamp tracking of content changes, no history panel or restore actions in SnippetManager.

### [MISSING] [high] Right-click AI Assistant (context-menu AI instruction dialog with cursor awareness)
- Java: fd94f81, SnippetAiAssistFactory, SnippetEditDialog context menu, sends full snippet + cursor offset/line/column to AI
- Rust-Stand: SnippetManager.tsx SnippetCodeEditor component has no onContextMenu handler; no AI dialog component for snippet editing
- Lücke: Java: MonacoEditorPane context menu includes AI instruction option, opens SnippetAiAssistFactory dialog showing cursor position. Dialog sends snippet+offset to AI for full-snippet rewrite. Rust: SnippetManager uses CodeMirror but has no context menu; no AI instruction dialog.
- Verifikation: SnippetCodeEditor uses CodeMirror without context menu integration. No AI instruction dialog for snippets, no cursor position tracking for AI requests, no MonacoEditorPane context menu equivalent in snippet editing UI.

### [MISSING] [high] AI review and rewrite flows (error review, improvement themes, custom instructions, alternative solutions, security fixes)
- Java: fd94f81 + a8e8323, SnippetAiReviewDialog.java, SnippetSecurityReportDialog.java, AlternativeSnippetSolutionsDialog (enhanced with selection support), structured AI responses, before/after preview dialogs
- Rust-Stand: SnippetManager.tsx lacks AIReviewDialog, SecurityReportDialog, or AlternativeSolutionsDialog components
- Lücke: Java: Multiple AI workflows - error review shows structured findings, improvement themes trigger rewrite, custom instruction dialog, alternative solutions dialog (selection-aware), security report dialog with selectable findings. All use before/after preview. Rust: Only AI metadata generation exists; no AI-driven code rewrite, review, or improvement workflows in SnippetManager.
- Verifikation: No SnippetAiReviewDialog, SnippetSecurityReportDialog, or AlternativeSnippetSolutionsDialog equivalents in Rust. SnippetManager only supports AI metadata generation (name/category/description). No structured AI workflows for code review, improvements, alternatives, or security analysis.

### [MISSING] [medium] Cursor-aware AI completion (suggestions as ghost text, click-to-insert)
- Java: fd94f81, SnippetEditDialog AI completion request at caret location, MonacoEditorPane ghost-text rendering
- Rust-Stand: SnippetManager.tsx has no completion request/ghost-text UI; snippet_commands.rs has no completion endpoint
- Lücke: Java: MonacoEditorPane integrates AI completion via JavaScript bridge, shows suggestions as ghost text inline, user clicks to accept. SnippetEditDialog can request completions at current caret. Rust: No AI completion feature; CodeMirror in SnippetManager is static without AI suggestion integration.
- Verifikation: CodeMirror is configured with autocompletion: false. No AI completion integration, no ghost text rendering, no inline suggestion UI in SnippetCodeEditor.

### [MISSING] [low] Session-local auto-completion toggle (enabled per editor session, off by default)
- Java: fd94f81, SnippetEditDialog session-scoped autoCompletionEnabled property, persisted per-session not per-snippet
- Rust-Stand: SnippetManager.tsx has no auto-completion settings
- Lücke: Java: SnippetEditDialog tracks autoCompletionEnabled per session, defaults to false, user can toggle in UI. When enabled, pauses trigger suggestions. Rust: No auto-completion feature exists.
- Verifikation: No auto-completion feature exists. CodeMirror's autocompletion is disabled and not user-configurable per session. No session-scoped autoCompletionEnabled property in SnippetManager state.

### [PARTIAL] [medium] Selected-code scope for AI workflows (alternative solutions, improvements operate on selection)
- Java: fd94f81, SnippetDiffSelectionSupport.java (new, 48 lines), AlternativeSnippetSolutionsDialog selection-aware request, falls back to whole snippet if no selection
- Rust-Stand: SnippetManager.tsx SnippetCodeEditor has no onSelection handler; no selection-scoped AI requests
- Lücke: Java: AI workflows check for selected text range, send only selected region for processing, fall back to full snippet if no selection. SnippetDiffSelectionSupport tracks selection boundaries. Rust: SnippetManager has no selection-based AI workflow support.
- Verifikation: AI request payload includes selectedText field (used in terminal AI actions), but snippet workflows don't leverage selection. SnippetManager.tsx passes full snippet.content to AI metadata generation, not selected text. No SnippetDiffSelectionSupport or selection-aware AI workflow switching logic.

### [PARTIAL] [medium] PlantUML diagram storage and management (persisted with snippet, marked stale by content hash)
- Java: src/main/java/de/kortty/model/SnippetDiagram.java (new), stores id, title, type, plantUmlSource, sourceContentSha256, customInstructions, codeReferences, createdAt, updatedAt
- Rust-Stand: src-tauri/src/model/snippet.rs:SnippetDiagram has minimal fields vs Java's comprehensive model
- Lücke: Java: SnippetDiagram model with full metadata, source content SHA-256, custom instructions, code references, timestamps. Rust: SnippetDiagram exists with id, name, diagram_type, source, rendered_path, content_hash but lacks title, customInstructions, codeReferences, timestamps. Rust model is simplified.
- Verifikation: SnippetDiagram model exists with id, name, diagram_type, source, rendered_path, content_hash. Missing from Rust: title, customInstructions, codeReferences, createdAt, updatedAt fields. Rust model is simplified compared to Java version.

### [MISSING] [low] Diagram code references and hotspots (PlantUML SVG includes clickable line references)
- Java: fd94f81, SnippetDiagramSupport.java adds code reference support, SnippetDiagram.codeReferences list, SnippetDiagramDialog renders hotspots in SVG
- Rust-Stand: src-tauri/src/model/snippet.rs:SnippetDiagram lacks codeReferences field; SnippetManager.tsx has no diagram hotspot UI
- Lücke: Java: PlantUML diagrams can include line-range references, rendered SVG has clickable hotspots with tooltips linking back to snippet lines. Rust: SnippetDiagram.source stores PlantUML, but no code-reference generation or SVG hotspot handling in Rust code.
- Verifikation: SnippetDiagram.source stores PlantUML, but no code-reference generation, no SVG hotspot/tooltip rendering, no SnippetDiagramSupport code reference logic in Rust.

### [MISSING] [low] Diagram controls (copy PlantUML source, save bg color, apply readable activity colors)
- Java: fd94f81, SnippetDiagramDialog context menu and buttons, ensureReadableActivityColors() in SnippetAiResponseSupport
- Rust-Stand: SnippetManager.tsx handleRenderPlantUml only calls build/render, no diagram management UI
- Lücke: Java: SnippetDiagramDialog has buttons to copy source, apply readable colors, save background. Rust: No diagram dialog; diagram rendering in SnippetManager is limited to build/render calls without interactive controls.
- Verifikation: No diagram dialog UI in Rust. SnippetManager only renders PlantUML and stores result path. No copy source button, no background color UI, no ensureReadableActivityColors() equivalent, no right-click context menu on diagrams.

### [MISSING] [low] Technical description dialog (comment-syntax descriptions, configurable line width, sentence splitting)
- Java: fd94f81, SnippetDescriptionDialog.java (enhanced), generates comments in snippet's language with line-width wrapping
- Rust-Stand: SnippetManager.tsx has description textarea but no AI-assisted description generation or comment formatting
- Lücke: Java: Dialog generates technical descriptions (block comments in chosen language syntax), wraps to configured line width, splits sentences. Rust: No technical description dialog or comment-generation feature in SnippetManager.
- Verifikation: No SnippetDescriptionDialog equivalent in Rust. No feature to generate block comments in snippet's language syntax with configurable line-width wrapping or sentence-splitting logic.

### [MISSING] [low] Save edited snippet as new (create new snippet from current editor state without overwriting original)
- Java: fd94f81, SnippetEditDialog 'Save as new' action, creates new Snippet with current content
- Rust-Stand: SnippetManager.tsx handleSave either updates existing or adds new; no save-as-new workflow
- Lücke: Java: SnippetEditDialog has 'Save as new' button when editing existing snippet, creates new entry without modifying original. Rust: SnippetManager save action overwrites or creates new; no 'save as new' option.
- Verifikation: SnippetManager save action overwrites existing snippets or creates new. No 'Save as new' button or clone action when editing existing snippet.

### [PARTIAL] [low] AI prompt hardening (rejection of hidden reasoning, think tags, invented files, external URLs, unsupported formats)
- Java: fd94f81, AiPromptBuilder and SnippetAiWorkflowSupport add explicit prompt instructions rejecting <think>, invented files, external URLs
- Rust-Stand: SnippetManager.tsx buildSnippetMetadataPrompt lacks safety clauses compared to Java's SnippetAiWorkflowSupport
- Lücke: Java: Prompts explicitly instruct model to reject <think>, invented files, external URLs, unsupported output formats. Rust: buildSnippetMetadataPrompt in SnippetManager.tsx does not include these safety instructions; prompt is simpler.
- Verifikation: buildSnippetMetadataPrompt in SnippetManager.tsx includes basic instructions (no Markdown, JSON schema constraints, language/category limits). But lacks explicit safety instructions rejecting <think> tags, invented files, external URLs, or unsupported formats. Simpler prompt than Java AiPromptBuilder.

### [MISSING] [low] Snippet AI structured response types (CompletionSuggestion, CodeReviewFinding, CodeImprovement, OneLinerSuggestion, SecurityFinding, PlantUmlDiagram records)
- Java: a8e8323, SnippetAiResponseSupport.java adds multiple record types with isUsable() validation methods and JSON parsing
- Rust-Stand: src/components/dialogs/SnippetManager.tsx has untyped JSON parsing; no response validation records
- Lücke: Java: Structured types for AI responses with built-in validation (e.g., CompletionSuggestion.isUsable checks non-blank insertText). Rust: No structured response types; parseSnippetMetadataSuggestion in SnippetManager.tsx only handles JSON object extraction without strong typing.
- Verifikation: No structured response type definitions in Rust. parseSnippetMetadataSuggestion in SnippetManager.tsx only extracts JSON name/category/description fields. No CompletionSuggestion, CodeReviewFinding, SecurityFinding records with isUsable() validation methods.

### [PARTIAL] [low] Snippet AI response parsing (markdown code blocks, JSON arrays/objects, multi-solution extraction with fallback)
- Java: a8e8323, SnippetAiResponseSupport.java parseCompletionSuggestion, parseAlternativeSolutions, parseSecurityFindings with robust JSON/markdown extraction and field-order-agnostic parsing (firstString, firstArray)
- Rust-Stand: SnippetManager.tsx parseSnippetMetadataSuggestion simpler than Java's multi-fallback strategy
- Lücke: Java: Complex parsing logic handles markdown code blocks, JSON structure variations, field aliasing (solutions/alternatives/alternativeSolutions), single-object fallback for arrays. Rust: parseSnippetMetadataSuggestion attempts markdown extraction and JSON fallback but lacks field-order flexibility.
- Verifikation: parseSnippetMetadataSuggestion in SnippetManager.tsx attempts markdown extraction (```json blocks) and JSON fallback. Handles single object extraction. But lacks field-order flexibility (firstString, firstArray), multi-solution extraction (solutions/alternatives/alternativeSolutions aliasing), and robust JSON structure variation handling of Java SnippetAiResponseSupport.

### [PARTIAL] [high] Code formatter service with provider states (built-in, bundled, external fallback, unavailable)
- Java: a8e8323, src/main/java/de/kortty/core/CodeFormatterService.java (876 lines), FormatterInfo record with providerType enum, ~20 language formatters with version constants
- Rust-Stand: src-tauri/src/snippet_tools.rs:format_snippet_code handles {json, xml}, external command; missing bash, python, ruby, java, c/c++, etc. explicit providers
- Lücke: Java: Comprehensive CodeFormatterService with ~20 language formatters (json, xml, yaml, bash, python, java, prettier, black, etc.), provider-state tracking (BUILT_IN, BUNDLED, EXTERNAL_FALLBACK, UNAVAILABLE). Rust: snippet_tools.rs format_snippet_code supports JSON, XML built-in, external command fallback, but lacks comprehensive language coverage and provider-state tracking.
- Verifikation: Rust's format_snippet_code supports JSON (built-in), XML/HTML (built-in), and external formatter commands. No comprehensive ~20 language formatters as Java has. No FormatterInfo record with providerType enum tracking (BUILT_IN, BUNDLED, EXTERNAL_FALLBACK, UNAVAILABLE). Language coverage limited to basic JSON/XML.

### [MISSING] [low] Formatter manifest pinning (formatter versions and sources in properties file)
- Java: fd94f81, src/main/resources/formatters/formatter-manifest.properties pins shfmt, Prettier, sql-formatter, Perl::Tidy, google-java-format versions
- Rust-Stand: No formatter manifest file in Rust project
- Lücke: Java: Pinned formatter versions in manifest file for reproducible builds. Rust: No formatter manifest or version pinning; external formatter commands are called as-is from PATH.
- Verifikation: No formatter-manifest.properties or version pinning in Rust. External formatters invoked as-is from PATH without reproducible version control.

### [MISSING] [medium] AI fallback for unavailable formatting (prompt AI when local tool unavailable, before/after preview)
- Java: fd94f81, SnippetEditDialog asks user to use AI formatting when tool unavailable, shows preview before applying
- Rust-Stand: SnippetManager.tsx handleFormatSnippet catches errors but does not offer AI fallback
- Lücke: Java: When formatter unavailable for language/width, offers AI-assisted formatting with preview. Rust: format_snippet command fails silently or with error; no AI fallback offered.
- Verifikation: format_snippet command in Rust fails with error when formatter unavailable. No AI-assisted formatting fallback, no preview dialog before applying AI formatting.

### [MISSING] [medium] Review windows with zoom and copy-to-clipboard (AI diff/review/alternative/security dialogs use readable code + zoom UI)
- Java: fd94f81, SnippetAiDiffDialog.java, SnippetAiReviewDialog.java, SnippetSecurityReportDialog.java, AlternativeSnippetSolutionsDialog with zoom buttons and Ctrl/Cmd+/- support, copy action in right-click menus
- Rust-Stand: SnippetManager.tsx has no AI result dialogs
- Lücke: Java: All AI result dialogs include zoom buttons (A-, A+) and keyboard shortcuts (Ctrl/Cmd ±), copy-to-clipboard on right-click, larger readable text. Rust: No AI result dialogs exist for snippet workflows.
- Verifikation: No review dialogs (SnippetAiDiffDialog, SnippetAiReviewDialog, SnippetSecurityReportDialog, AlternativeSnippetSolutionsDialog equivalents) in Rust. No zoom buttons (A-/A+), no Ctrl/Cmd +/- shortcuts, no copy-to-clipboard functionality in snippet AI result dialogs.

### [MISSING] [low] Column ruler marker context menu (format-to-limit, clear-limit actions)
- Java: fd94f81, SnippetColumnRuler.java context menu with formatToLimitItem and clearLimitItem MenuItem entries
- Rust-Stand: No ruler in Rust SnippetManager
- Lücke: Java: SnippetColumnRuler context menu (right-click on marker) offers format-to-limit and clear-limit. Rust: No ruler component, no context menu.
- Verifikation: No ruler component exists. No context menu with formatToLimitItem and clearLimitItem MenuItem entries.

## Bereich: terminal-recording (26 Lücken)

### [PARTIAL] [high] Replay event streaming (snapshot, input, pause/resume metadata)
- Java: TerminalRecordingSession.recordScreenSnapshot(), recordUserInputActivity(), auto_pause events (~250 lines)
- Rust-Stand: src-tauri/src/terminal_recording.rs lines 97-131, 162-173
- Lücke: Rust supports snapshot and input events in JSON format. MISSING: Java's auto_pause and auto_resume events that track idle timeout detection and resumption source, user_input_activity events, session_created/session_closed metadata.
- Verifikation: Rust supports snapshot (lines 104-114) and input events (lines 117-130) in JSON format with atMillis timestamps. However, MISSING: Java's auto_pause and auto_resume event types (Java TerminalRecordingSession.java:69 schedules idle check that writes 'auto_pause' events; Rust has NO background idle monitor, so no auto_pause/auto_resume events are generated). Also MISSING: session_created/session_closed metadata events (Java writes these at line 65-68, Rust uses generic 'metadata' event). MISSING: user_input_activity distinction (Java has recordUserInputActivity() method for specific input tracking).

### [MISSING] [high] Auto-pause on idle with configurable idle timeout
- Java: TerminalRecordingSession.checkIdle(), TerminalRecordingState.AUTO_PAUSED, idlePauseMillis property
- Rust-Stand: src-tauri/src/model/settings.rs has terminal_recording_idle_auto_pause field but no implementation in terminal_recording.rs
- Lücke: Java has background ScheduledExecutorService that checks idle every 1 second and automatically pauses recording after idlePauseSeconds with auto_pause events. Rust has pause/resume API and settings field but NO background idle monitor, NO idle-triggered state transitions, NO auto_pause/auto_resume events in replay.
- Verifikation: Rust has setting field (terminal_recording_idle_auto_pause in settings.rs line 168, defaulting to true) but NO IMPLEMENTATION. Rust TerminalRecordingStore has no idle monitor thread, no checkIdle() equivalent, and pause/resume are only triggered via explicit API calls. Java TerminalRecordingSession.java:37 has ScheduledExecutorService and line 69 schedules idleExecutor.scheduleAtFixedRate(this::checkIdle, 1, 1, TimeUnit.SECONDS) which automatically pauses recording after idlePauseMillis (line 179-183). No auto_pause/auto_resume events written to replay in Rust.

### [PARTIAL] [medium] Terminal geometry capture (columns, rows, pixel dimensions)
- Java: TerminalRecordingScreenSnapshot (columns, rows, pixelWidth, pixelHeight fields)
- Rust-Stand: src-tauri/src/terminal_recording.rs lines 72-73, 108-109; model missing pixelWidth/pixelHeight
- Lücke: Rust captures columns and rows in snapshot events. Java also captures pixelWidth and pixelHeight for proper video rendering layout. Rust's export_video() uses DEFAULT_WIDTH/HEIGHT constants instead of capturing actual geometry.
- Verifikation: Rust captures columns and rows in snapshot events (terminal_recording.rs lines 108-109). MISSING: pixelWidth and pixelHeight fields. Rust export_video() uses hardcoded DEFAULT_WIDTH (1280) and DEFAULT_HEIGHT (720) constants (lines 21-22, 285-286) instead of capturing actual geometry. Java TerminalRecordingScreenSnapshot (referenced in TerminalRecordingSession.java:128-132) includes pixelWidth and pixelHeight fields that are written to replay JSON and used in RenderLayout calculation.

### [MISSING] [medium] Per-cell color and style metadata (StyleRun with foreground, background, text options)
- Java: TerminalRecordingStyleRun record (row, column, text, foreground, background, options list), TerminalRecordingScreenSnapshot.styleRuns
- Rust-Stand: src-tauri/src/model/terminal_recording.rs has no styleRuns or color fields
- Lücke: Java fully supports per-cell color capture with StyleRun objects (foreground color #hex, background color, options like BOLD). Rust has NO StyleRun structure, NO color metadata in replay events, no UI for 'capture colors' setting.
- Verifikation: Rust has NO StyleRun structure, NO color fields in TerminalRecordingReplayEvent type (src/types/terminalRecording.ts), and NO color capture in recording or playback. Rust write_snapshot_ppm() applies fixed colors based on character type (digits=orange, punctuation=blue, text=cyan) but does NOT capture terminal's actual ANSI colors. Java TerminalRecordingSession.java:134-150 writes styleRuns array with row, column, text, foreground (hex color), background (hex color), and options array for each styled region.

### [MISSING] [medium] Color capture toggle (terminalRecordingCaptureColorsEnabled setting)
- Java: GlobalSettings.terminalRecordingCaptureColorsEnabled, SettingsDialog checkbox, export options dialog uses it
- Rust-Stand: src/store/settingsStore.ts and SettingsDialog.tsx have no terminalRecordingCaptureColors checkbox
- Lücke: Java has global setting to enable/disable color capture to keep replay files small (default: false). Rust has no such setting or UI control.
- Verifikation: Rust has NO such setting in GlobalSettings (checked model/settings.rs) and NO UI control in SettingsDialog.tsx. Java GlobalSettings and SettingsDialog have checkbox for terminalRecordingCaptureColorsEnabled (referenced in TerminalRecordingManagerDialog.java:466 showExportOptionsDialog with hasColorData parameter).

### [PARTIAL] [high] Replay loading and event parsing
- Java: TerminalRecordingService.loadReplayFrames(), builds TerminalRecordingReplayFrame list with timing
- Rust-Stand: src-tauri/src/terminal_recording.rs lines 233-238, 360-392
- Lücke: Rust loads replay events and parses JSON. Java parses events AND builds timed replay frames (duration calculation, pause/resume filtering, frame slicing). Rust returns raw events without frame timing.
- Verifikation: Rust loads replay events and parses JSON line-by-line (terminal_recording.rs:360-391 read_replay_events). MISSING: frame timing/duration calculation. Rust returns raw TerminalRecordingReplayEvent list without timing metadata. Java TerminalRecordingService.loadReplayFrames() builds TerminalRecordingReplayFrame list WITH duration calculations from event timestamps (referenced in line 373-376), filtering paused intervals, creating timed replay frames with frame.durationSeconds().

### [MISSING] [medium] Replay timeline and frame indexing (TerminalRecordingReplayTimeline)
- Java: TerminalRecordingReplayTimeline (76 lines): frameIndexAt(), clampSeconds(), frame access, duration tracking
- Rust-Stand: No equivalent in Rust codebase
- Lücke: Java provides timeline structure for seeking and frame lookup by timestamp, with binary search for efficiency. Rust returns raw event list without timeline abstraction.
- Verifikation: Rust has NO timeline abstraction layer. Rust returns raw event list (TerminalRecordingReplayFile.events: Vec<TerminalRecordingReplayEvent>). Java provides TerminalRecordingReplayTimeline (76 lines) with frameIndexAt() binary search for O(log n) seeking, clampSeconds(), frame access, totalDurationSeconds tracking. Used in TerminalRecordingReplayDialog line 28 and referenced throughout playback logic.

### [MISSING] [medium] Time range selection for video export (start/end seconds)
- Java: TerminalRecordingTimeRange record, export options dialog with start/end fields
- Rust-Stand: src-tauri/src/model/terminal_recording.rs TerminalRecordingVideoExportOptions has no timeRange field
- Lücke: Java allows custom time ranges in export (default: all). Rust export options lack timeRange field.
- Verifikation: Rust TerminalRecordingVideoExportOptions (model/terminal_recording.rs:97-105) has NO timeRange, startSeconds, or endSeconds fields. Export is triggered directly without options dialog (TerminalRecordingManagerDialog.tsx:82-102 exportVideo calls invoke directly with format only). Java TerminalRecordingExportOptions and showExportOptionsDialog (lines 417-508) include TerminalRecordingTimeRange with parseExportOptions allowing custom start/end MM:SS fields.

### [MISSING] [low] Time jump parser (MM:SS and minute formats)
- Java: TerminalRecordingTimeJumpParser (58 lines), parseMinutesAndSeconds(), parseMinutes()
- Rust-Stand: No equivalent in Rust
- Lücke: Java parses replay viewer time-jump field accepting 'MM:SS' or minute formats. No Rust equivalent.
- Verifikation: Rust has NO time jump parser equivalent. No component or backend function parses MM:SS or minute formats for seeking. Java TerminalRecordingTimeJumpParser (58 lines) with parseSeconds(), parseMinutesAndSeconds(), parseMinutes() used in replay dialog for time-jump field seeking.

### [MISSING] [high] In-app replay viewer dialog with timeline slider
- Java: TerminalRecordingReplayDialog (308 lines): TextArea for screen display, Slider for timeline, play/pause/stop controls, speed spinner
- Rust-Stand: src/components/dialogs/TerminalRecordingManagerDialog.tsx lines 163-167 shows last snapshot only
- Lücke: Java has full replay viewer with live playback animation, timeline slider, speed control (1x-20x). Rust shows only last snapshot in a pre element, no viewer UI.
- Verifikation: Rust shows ONLY last snapshot in pre element (TerminalRecordingManagerDialog.tsx:163-167 displays last snapshot text). NO playback UI, NO timeline slider, NO play/pause/stop controls, NO speed spinner. Java TerminalRecordingReplayDialog (308 lines) with TextArea for screen display, Slider for timeline (line 27 timelineSlider reference), play/pause buttons, speed spinner (lines 34, 106-108).

### [MISSING] [high] Replay playback with frame timing and speed control
- Java: TerminalRecordingReplayDialog.updatePlayback(), AnimationTimer, playbackSpeedMultiplier (1x to 20x)
- Rust-Stand: No playback UI or logic in Rust components
- Lücke: Java implements real-time playback animation, frame duration-based timing, speed multiplier. Rust has no playback implementation.
- Verifikation: Rust has NO playback implementation. TerminalRecordingManagerDialog displays only static last snapshot. Java TerminalRecordingReplayDialog.updatePlayback() (referenced lines 177-180) implements real-time playback animation with frame duration-based timing, speed multiplier property (playbackSpeedMultiplier 1x to 20x), AnimationTimer integration.

### [MISSING] [high] Timeline slider seeking with live position updates
- Java: TerminalRecordingReplayDialog.timelineSlider, beginSliderSeek(), setPlaybackPosition()
- Rust-Stand: TerminalRecordingManagerDialog.tsx has no slider or seek controls
- Lücke: Java replay viewer has interactive slider with frame-accurate seeking and continuous position label updates.
- Verifikation: Rust has NO timeline slider or seeking. Manager dialog only shows last snapshot. Java TerminalRecordingReplayDialog has timelineSlider property (line 27), beginSliderSeek(), setPlaybackPosition() (line 285) with continuous position label updates during drag.

### [MISSING] [medium] Time jump field in replay viewer
- Java: TerminalRecordingReplayDialog.timeJumpField with MM:SS placeholder, seekToTimeJumpInput()
- Rust-Stand: TerminalRecordingManagerDialog.tsx has no time jump field
- Lücke: Java allows entering time like '1:30' to jump to that point in replay. Rust has no time jump input.
- Verifikation: Rust manager dialog has NO time jump input field. Java TerminalRecordingReplayDialog.timeJumpField with MM:SS placeholder and seekToTimeJumpInput() method uses TerminalRecordingTimeJumpParser to jump to specific time.

### [PARTIAL] [medium] Video export to WebM/VP9 and MKV/FFV1 formats
- Java: TerminalRecordingExportFormat enum (WEBM, MKV), ffmpeg integration with codec selection
- Rust-Stand: src-tauri/src/model/terminal_recording.rs TerminalRecordingExportFormat enum, export_video() function
- Lücke: Rust supports Webm and Mkv export formats. Java additionally supports different codec pairs (WebM/VP9 vs MKV/FFV1 display names).
- Verifikation: Rust supports Webm and Mkv export formats (TerminalRecordingExportFormat enum in model/terminal_recording.rs:17-20, and exportVideo uses ffmpeg without codec specification). However, the claim about 'different codec pairs' is misleading - ffmpeg command doesn't specify codecs, just uses defaults. Java TerminalRecordingExportFormat enum mentions codec pairs in display names (WebM/VP9 vs MKV/FFV1) but actual implementation not verified here.

### [PARTIAL] [medium] FFmpeg availability detection and configuration
- Java: GlobalSettings.terminalRecordingFfmpegPath, TerminalRecordingService.isFfmpegAvailable()
- Rust-Stand: src-tauri/src/terminal_recording.rs ffmpeg_availability() uses find_tool from PATH only
- Lücke: Rust detects ffmpeg from PATH. Java also allows explicit path configuration in settings with UI browse button.
- Verifikation: Rust detects ffmpeg from PATH via find_tool() function (terminal_recording.rs:588-593, ffmpeg_availability() at lines 260-266). MISSING: explicit path configuration setting in GlobalSettings and NO UI browse button in SettingsDialog. Java GlobalSettings.terminalRecordingFfmpegPath allows explicit path configuration with SettingsDialog UI control.

### [PARTIAL] [medium] Video export with frame rendering
- Java: TerminalRecordingService.renderFrameImages() (~250 lines), resolveRenderLayout(), RenderLayout calculation
- Rust-Stand: src-tauri/src/terminal_recording.rs lines 400-432, basic PPM rendering
- Lücke: Rust renders frames to PPM format with basic character coloring. Java renders with Font metrics, proper character bounds, padding calculation.
- Verifikation: Rust renders frames to PPM format (write_snapshot_ppm at lines 400-431) with basic character coloring (draw_cell lines 434-456 assigns fixed colors by character type). MISSING: Font metrics, proper character bounds, padding calculation. Java renderFrameImages() (~250 lines) uses Graphics2D with Font metrics, glyph bounds, proper character positioning, padding calculation as shown in RenderLayout class.

### [PARTIAL] [high] Video export with ffmpeg and progress tracking
- Java: TerminalRecordingService.exportReplay(), Task<Path>, ExportProgress callbacks with phase/current/total
- Rust-Stand: src-tauri/src/terminal_recording.rs lines 268-329, no progress callback mechanism
- Lücke: Rust calls ffmpeg synchronously without progress reporting. Java tracks PREPARING, RENDERING, ENCODING phases with estimated time remaining.
- Verifikation: Rust calls ffmpeg synchronously without progress reporting (terminal_recording.rs:313-323 Command::new(ffmpeg).status() blocks). MISSING: progress callbacks, phase tracking (PREPARING/RENDERING/ENCODING), estimated time remaining. Java TerminalRecordingService.exportReplay() uses Task<Path> with ExportProgress callbacks (line 389-391) tracking phase and remaining time through progressListener.

### [MISSING] [high] Export options dialog (format, time range, color inclusion)
- Java: TerminalRecordingManagerDialog.showExportOptionsDialog() (~60 lines), ComboBox, CheckBox, TextField for ranges
- Rust-Stand: TerminalRecordingManagerDialog.tsx exportVideo() calls invoke() directly without options dialog
- Lücke: Java shows dialog allowing selection of export format, custom time range (start/end MM:SS fields), and color inclusion checkbox. Rust export is triggered directly without options dialog.
- Verifikation: Rust export is triggered directly without options dialog (TerminalRecordingManagerDialog.tsx:82-102). NO format selection dialog, NO time range fields, NO color inclusion checkbox. Java TerminalRecordingManagerDialog.showExportOptionsDialog() (~60 lines) displays dialog with ComboBox for format, TextField for range start/end in MM:SS, CheckBox for includeColor.

### [MISSING] [medium] Export progress dialog with phase and ETA
- Java: TerminalRecordingManagerDialog.createExportProgressDialog(), exportProgressMessage() with phase and remaining time
- Rust-Stand: TerminalRecordingManagerDialog.tsx line 90 shows simple status message, no progress dialog
- Lücke: Java shows modal progress dialog with current phase (PREPARING/RENDERING/ENCODING), progress bar, and estimated remaining time. Rust has no progress UI.
- Verifikation: Rust has no progress UI. Export is synchronous blocking call without user feedback. Java TerminalRecordingManagerDialog.createExportProgressDialog() shows modal dialog with current phase (PREPARING/RENDERING/ENCODING) from ExportProgress, progress bar bound to task.progressProperty(), and estimated remaining time via exportProgressMessage().

### [PARTIAL] [high] Video Manager UI (TerminalRecordingManagerDialog)
- Java: TerminalRecordingManagerDialog (653 lines): list view, settings form, action buttons with icons
- Rust-Stand: src/components/dialogs/TerminalRecordingManagerDialog.tsx
- Lücke: Rust has basic manager dialog (list, delete, export to WebM/MKV). Java has extensive UI with recording settings (enabled, path, format, scope, color capture, auto-pause, ffmpeg path, idle threshold) and recording control buttons.
- Verifikation: Rust has basic manager dialog (TerminalRecordingManagerDialog.tsx 180 lines: list view, delete, export to WebM/MKV). MISSING: extensive recording settings form (enabled, path, format, scope, color capture, auto-pause, ffmpeg path, idle threshold), recording control buttons (Start/Stop), settings integration in main UI. Rust SettingsDialog.tsx does NOT include recording settings UI controls.

### [PARTIAL] [high] Settings integration (terminalRecordingEnabled, default format, scope, ffmpegPath)
- Java: GlobalSettings (8 recording-related fields), SettingsDialog checkbox and input controls
- Rust-Stand: src/store/settingsStore.ts has terminalRecordingEnabled, terminalRecordingIdleAutoPause, terminalRecordingDirectory but SettingsDialog.tsx has NO controls for them
- Lücke: Java stores and UI-editable: terminalRecordingEnabled, terminalRecordingStoragePath, terminalRecordingFormat (KORTTY_REPLAY/WEBM), terminalRecordingDefaultScope (ACTIVE_SPLIT/WHOLE_TAB), terminalRecordingCaptureColorsEnabled, terminalRecordingAutoPauseEnabled, terminalRecordingIdlePauseSeconds, terminalRecordingFfmpegPath. Rust has settings fields but NO SettingsDialog UI controls.
- Verifikation: Rust GlobalSettings (model/settings.rs:166-170) has terminal_recording_enabled (bool), terminal_recording_idle_auto_pause (bool), and terminal_recording_directory (Option<String>). MISSING in both settings model AND SettingsDialog UI: terminalRecordingFormat (default WEBM vs KORTTY_REPLAY), terminalRecordingDefaultScope (ACTIVE_SPLIT vs WHOLE_TAB), terminalRecordingCaptureColorsEnabled, terminalRecordingFfmpegPath (explicit path config). NO SettingsDialog.tsx controls for any recording settings.

### [MISSING] [medium] Recording trigger (start/stop keyboard shortcut Ctrl/Cmd+Shift+E)
- Java: MainWindow menu item 'Tools > Start/Stop Terminal Recording', keyboard shortcut Ctrl+Shift+E (Windows/Linux) or Cmd+Shift+E (Mac)
- Rust-Stand: No keyboard shortcut or menu action in Rust for terminal recording
- Lücke: Java has menu item and keyboard shortcut. Rust has no keyboard shortcut binding or Tools menu entry.
- Verifikation: Rust handleKeyDown() function in MainWindow.tsx (lines 3693-3782) has NO case for Ctrl+Shift+E or Cmd+Shift+E. MenuBar has 'Terminal Recordings...' menu item (line 122) to open manager dialog, but NO start/stop recording shortcut or menu item. Java MainWindow has 'Tools > Start/Stop Terminal Recording' menu with Ctrl+Shift+E (Windows/Linux) or Cmd+Shift+E (Mac) keyboard shortcut binding.

### [MISSING] [low] Recording status indicator in terminal bar
- Java: TerminalTab UI shows recording indicator when session is active
- Rust-Stand: TerminalTab.tsx has recordingSessionId prop but no visual indicator in UI
- Lücke: Java shows visual recording indicator in terminal tab/bar when recording is active.
- Verifikation: Rust has NO visual recording indicator when session is active. TerminalTab.tsx accepts recordingSessionId prop (line 45) but does NOT render any indicator badge/dot/icon showing recording status. Java TerminalTab UI displays recording indicator when session is active.

### [MISSING] [low] File snapshot duplicate detection
- Java: TerminalRecordingSession.recordScreenSnapshot() checks lastScreenByWidget.get() to skip unchanged snapshots
- Rust-Stand: src-tauri/src/terminal_recording.rs append_snapshot() has no equality check
- Lücke: Java deduplicates identical consecutive snapshots to keep replay files smaller. Rust records every snapshot without deduplication.
- Verifikation: Rust TerminalRecordingStore.append_snapshot() (lines 97-115) records every snapshot WITHOUT deduplication check. Java TerminalRecordingSession.recordScreenSnapshot() (lines 105-118) checks lastScreenByWidget.get(safeWidgetId) to skip unchanged snapshots (line 113: if equals, returns without recording).

### [MISSING] [low] Frame slicing by time range
- Java: TerminalRecordingService.sliceReplayFrames() filters frames by TerminalRecordingTimeRange
- Rust-Stand: No slicing implementation in Rust
- Lücke: Java can slice frame list to a custom time range for partial export. Rust has no frame slicing logic.
- Verifikation: Rust has NO frame slicing logic in terminal_recording.rs. Java TerminalRecordingService.sliceReplayFrames() filters frame list by TerminalRecordingTimeRange to support partial exports.

### [MISSING] [high] Timed replay frame building (frame duration calculation from replay events)
- Java: TerminalRecordingService.buildTimedReplayFrames() calculates frame durations from event timestamps, handles pause/resume
- Rust-Stand: src-tauri/src/terminal_recording.rs load_replay() returns raw events without durations
- Lücke: Java parses replay events to build TerminalRecordingReplayFrame objects with accurate duration_seconds calculated from gaps between events, skipping paused intervals. Rust returns raw events without timing information.
- Verifikation: Rust returns raw replay events without timing metadata (read_replay_events lines 360-391 parses JSON, returns TerminalRecordingReplayEvent list with at_millis but no frame duration calculation). Java TerminalRecordingService.buildTimedReplayFrames() parses replay events to build TerminalRecordingReplayFrame objects with accurate duration_seconds calculated from gaps between event timestamps, skipping paused intervals (referenced in line 336).

## Bereich: filebrowser-emulation (13 Lücken)

### [PARTIAL] [medium] File browser context menu with open/copy/cut/paste/delete
- Java: src/main/java/de/kortty/ui/LocalFileBrowser.java (aea0a18) - createContextMenu, copySelectedFiles, pasteFiles, deleteSelectedFiles methods
- Rust-Stand: src/components/files/LocalFileBrowser.tsx has minimal context menu - only edit capability. Backend supports list_local_dir, read_local_text_file, write_local_text_file, local_delete, local_mkdir, local_rename via src-tauri/src/commands/sftp_commands.rs
- Lücke: Missing: copy/cut/paste operations, rename through context menu, new file/folder, archive operations

### [MISSING] [medium] Show/hide hidden files toggle
- Java: src/main/java/de/kortty/ui/LocalFileBrowser.java (aea0a18) - showHiddenFiles boolean, shouldShowPath, CheckMenuItem showHiddenMenuItem
- Rust-Stand: src/components/files/LocalFileBrowser.tsx lists all files without filtering or toggle for hidden files
- Lücke: No UI toggle to show/hide hidden files (starting with dot on Unix). Backend has no filter mechanism implemented.

### [MISSING] [medium] File permissions and ownership editor
- Java: src/main/java/de/kortty/ui/LocalFileBrowser.java (aea0a18) - setOwnerPermissionsDialog, setOwner, setGroup, setPermissions, octalToPosix, permissionsToOctal, isValidOctalPermissions; test: LocalFileBrowserPermissionsTest.java
- Rust-Stand: src/components/files/LocalFileBrowser.tsx has no owner/permission editor UI. Backend has no chmod/chown commands.
- Lücke: Missing: dialog to edit octal permissions (755/640 style), set file owner/group, read /etc/passwd and /etc/group for principal lookup, POSIX file attribute support

### [MISSING] [low] Archive creation (ZIP and TAR formats)
- Java: src/main/java/de/kortty/ui/LocalFileBrowser.java (aea0a18) - archiveSelected, archiveToZip, archiveToTar, addToTar, writeTarOctal methods
- Rust-Stand: No archive functionality in src/components/files/LocalFileBrowser.tsx or backend
- Lücke: Missing: context menu option to create ZIP or TAR/TAR.BZ2 archives of selected files, with proper TAR octal header formatting

### [MISSING] [low] File details view/properties dialog
- Java: src/main/java/de/kortty/ui/LocalFileBrowser.java (aea0a18) - showDetails method, getPermissions, formatSize, displays modification time
- Rust-Stand: No file details/properties dialog in Rust implementation
- Lücke: Missing: dialog showing file size, modification date, permissions, owner, path for selected file(s)

### [MISSING] [high] Per-connection terminal emulation type selection (TERM value)
- Java: src/main/java/de/kortty/core/TerminalEmulationSupport.java (aea0a18) with availableEmulations(), fromStoredValue(), fromConnection(), termName(); src/main/java/de/kortty/model/ServerConnection.java - terminalEmulationType field; test: TerminalEmulationSupportTest.java
- Rust-Stand: src-tauri/src/model/connection.rs ConnectionSettings struct has no terminal_emulation_type field. src-tauri/src/ssh/session.rs line 395 hardcodes 'xterm-256color'
- Lücke: Missing: storage field for TERM value in ConnectionSettings, UI selector in QuickConnect/ConnectionEditor/ConnectionManager (like Java's TerminalEmulationComboBoxSupport), and dynamic TERM value sent to PTY when opening SSH session. Java supports full EmulationType enum (VT100, VT220, XTERM, WY60, HP700_92, etc.).

### [MISSING] [high] Terminal emulation type combo box with searchable selection
- Java: src/main/java/de/kortty/ui/TerminalEmulationComboBoxSupport.java (aea0a18) - configureComboBox, searchable FilteredList, editable selection with displayName and termName matching
- Rust-Stand: No terminal emulation selector UI in src/components/dialogs/ConnectionManager.tsx, ConnectionEditor, or QuickConnect
- Lücke: Missing: ComboBox for selecting terminal type by enum name, TERM value, or display label with live search filtering

### [MISSING] [high] Drag-and-drop file upload with progress and directory resolution
- Java: src/main/java/de/kortty/ui/TerminalView.java (aea0a18 and later commits) - handleFileDragDropped, uploadOne, resolveDragDropRemoteDirectory, resolveSftpStartDirectory, needsSftpStartDirectory; test: TerminalViewDragDropPathTest.java
- Rust-Stand: No file drag-drop upload to terminal in src/components/terminal/TerminalTab.tsx or MainWindow.tsx. SFTP upload exists (sftp_upload command) but no drag-drop triggering it for terminal files
- Lücke: Missing: drag-and-drop file upload to SSH session with resolution of ~/ paths through SFTP start directory, progress dialog showing upload target, elapsed time, current filename, and byte count. Java supports tilde-relative path normalization (~/dir resolves to home directory + subpath)

### [PARTIAL] [medium] Working directory extraction from prompt heuristics (space-separated and bracketed)
- Java: src/main/java/de/kortty/ui/TerminalView.java (aea0a18) - extractWorkingDirectoryFromPromptLine with stripPromptDirectoryDecorations; test: TerminalViewShortcutHeuristicsTest.java added extractsHomeRelativeWorkingDirectoryFromSpaceSeparatedPrompt and extractsHomeRelativeWorkingDirectoryFromBracketedSpaceSeparatedPrompt
- Rust-Stand: src/components/terminal/TerminalTab.tsx has OSC 7 tracking and typed directory command parsing (agent shortcut patterns), but TerminalView.tsx does not extract from prompt line for bracketed/space-separated formats
- Lücke: Partial: supports absolute directory extraction from OSC 7 and typed cd commands, but missing heuristic extraction from prompt text like 'daniel@fedora ~/Dokumente $' or '[daniel@fedora ~/Dokumente]$' to infer working directory

### [MISSING] [medium] Drag-drop target directory normalization (handling ~ and ~/ paths)
- Java: src/main/java/de/kortty/ui/TerminalView.java (aea0a18) - resolveDragDropRemoteDirectory static method with tilde normalization, normalizeAbsoluteRemotePath, isTildeRemoteDirectoryHint; test: TerminalViewDragDropPathTest.java
- Rust-Stand: No corresponding path normalization in Rust frontend or backend for drag-drop scenarios
- Lücke: Missing: resolution of tilde-relative paths (~/subdir) during drag-drop upload - must resolve home directory from SFTP start directory query

### [PARTIAL] [medium] Directory tracking updates from SSH connection remote cwd hints
- Java: src/main/java/de/kortty/core/SshTtyConnector.java (aea0a18) - updateHomeRemoteDirectoryHint, resolveRemoteDirectoryHint, normalizeAbsoluteRemotePath for home directory tracking
- Rust-Stand: src-tauri/src/ssh/session.rs tracks working directory from OSC 7 but has no explicit home directory tracking separate from current directory
- Lücke: Partial: OSC 7 directory tracking exists, but missing explicit updateHomeRemoteDirectoryHint for tracking home directory separately from current working directory (used for ~/ path expansion)

### [MISSING] [medium] Terminal drag-drop progress dialog with target, elapsed time, and filename display
- Java: src/main/java/de/kortty/ui/TerminalView.java (aea0a18+) - dialog with targetLabel, timeLabel, statusLabel, currentFileLabel, progressBar; started at startTime.get()
- Rust-Stand: No file upload dialog in Rust terminal views
- Lücke: Missing: upload progress UI showing remote target directory, elapsed time in seconds, current filename being uploaded, progress bar, and abort button

### [MISSING] [high] Terminal emulation stored in ServerConnection model
- Java: src/main/java/de/kortty/model/ServerConnection.java (aea0a18) - terminalEmulationType field with getter/setter, defaults to XTERM
- Rust-Stand: src-tauri/src/model/connection.rs ConnectionSettings struct lacks terminal_emulation_type field
- Lücke: Missing: storage field and persistence for terminal emulation type in connection configuration XML/JSON

## Bereich: ai-services (21 Lücken)

### [MISSING] [high] AI CLI provider registry and descriptor metadata
- Java: commit 62a87d0, src/main/java/de/kortty/core/AiCliProviderRegistry.java (160 lines); AiCliProviderDescriptor, AiCliArgumentPreset, AiCliModelPreset classes
- Lücke: Java: Registry of 17 known AI CLI providers (Claude Code, Codex, Devin, Gemini, OpenCode, etc.) with command-candidate lists, model presets with reasoning effort levels, and argument presets. Rust has no equivalent—profiles only support HTTP API mode with api_url, model, api_key. Local CLI provider support is completely absent from Rust.
- Verifikation: Rust AiProfile (src-tauri/src/model/ai.rs, lines 159-220) has no cliProviderId, cliExecutablePath, or cliArgumentsTemplate fields. No enum or registry equivalent to Java's AiCliProviderRegistry, AiCliProviderDescriptor, AiCliArgumentPreset, or AiCliModelPreset classes found anywhere in the codebase.

### [MISSING] [high] AI CLI argument template parser with placeholder substitution
- Java: src/main/java/de/kortty/core/AiCliArgumentTemplate.java (145 lines), handles {model}, {reasoning}, {promptFile}, {systemPromptFile}, {userPromptFile}, {stdinPrompt}
- Lücke: Java: Parses and expands CLI argument templates supporting line-based and inline argument syntax with quote/escape handling. Substitutes provider variables before CLI execution. No equivalent in Rust—LocalCliAiService is not ported.
- Verifikation: No Rust service handles CLI argument template parsing, expansion of {model}, {reasoning}, {promptFile}, {systemPromptFile}, {userPromptFile}, {stdinPrompt} placeholders, or quote/escape handling for line-based/inline argument syntax.

### [MISSING] [high] LocalCliAiService: execute AI prompts through local CLI provider
- Java: src/main/java/de/kortty/core/LocalCliAiService.java (v2.2.0 refined), extends AiPromptService with CLI provider execution, skill appending, timeout handling
- Lücke: Java: Spawns local CLI process with expanded arguments, pipes prompts to stdin/files, captures stdout, times out after configurable duration. Supports skill prompt appending for chat and agent. Rust's ai::mod.rs only sends HTTP requests; no local CLI mode implemented.
- Verifikation: Rust ai::execute_request() always sends HTTP requests. No fallback to local CLI execution mode exists. Terminal agent also does not use CLI providers for AI prompting.

### [MISSING] [high] AiConnectionMode enum: HTTP_API vs LOCAL_CLI
- Java: src/main/java/de/kortty/model/AiConnectionMode.java; AiProfile.connectionMode field with getter/setter
- Lücke: Java: AiProfile stores connectionMode to switch between HTTP API and local CLI execution. AiProfile also adds cliProviderId, cliExecutablePath, cliArgumentsTemplate fields. Rust AiProfile lacks all CLI-related fields and mode selection; only api_url + model + api_key for HTTP.
- Verifikation: AiProfile struct (lines 159-220 in model/ai.rs) has no connectionMode, cliProviderId, cliExecutablePath, or cliArgumentsTemplate fields. Mode selection is implicit (always HTTP) and not configurable.

### [PARTIAL] [high] LocalLmModelResolver: LM Studio auto model selection
- Java: src/main/java/de/kortty/core/LocalLmModelResolver.java (commit 70e95b9), static methods canResolve, loadLoadedLlmModelKeys, resolve with AUTO/MANUAL modes, selectAutoModel logic
- Lücke: Java: Detects local LM Studio /api/v1/models endpoint, fetches loaded model list via HTTP GET, auto-selects first compatible model or preference. Rust has list_lm_studio_models command in ai_commands.rs and AiModelSelectionMode enum with Auto/Manual, but lacks the resolver logic to auto-select among loaded models. Rust model_selection_mode field exists but is not actively used to resolve models.
- Verifikation: PARTIAL IMPLEMENTATION: Rust supports LM Studio auto-detection and model listing via list_local_lm_models_with_client() and resolve_effective_model(). However, auto-selection logic is limited: when Auto mode is active, resolve_effective_model() fetches models and either returns the single loaded model or errors if multiple models exist (line 625-632). No 'selectAutoModel' logic to pick a preferred model from multiple options. The resolver does not cache or prefer specific model names when ambiguous. AiProfile.model_selection_mode field exists but the auto-resolution happens only during dispatch, not persistently cached.

### [MISSING] [medium] AiReasoningDiscoveryService: probe and discover supported reasoning efforts
- Java: src/main/java/de/kortty/core/AiReasoningDiscoveryService.java (new in v2.2.0), discover() method probes NONE/MINIMAL/LOW/MEDIUM/HIGH/XHIGH and builds accepted list
- Lücke: Java: Issues connection-test requests with each reasoning effort candidate to verify API support. Builds a list of accepted reasoning efforts. Rust has AiReasoningEffort enum and AiProfile.reasoning_effort field, but no discovery service—no mechanism to probe which reasoning modes the configured API actually supports.
- Verifikation: Rust has AiReasoningEffort enum (model/ai.rs, lines 44-78) and AiProfile.reasoning_effort field, but no discovery mechanism. No service probes the API with different reasoning effort levels to determine what the API actually supports. No caching of discovery results or discovery keys to avoid re-probing.

### [PARTIAL] [medium] AiProfileSelectionSupport: profile lookup and default profile management
- Java: src/main/java/de/kortty/core/AiProfileSelectionSupport.java (new in v2.2.0), defaultProfile(), findById(), findByLookup(), reorderByRequestedOrDefault(), normalizeDefaultProfileId()
- Lücke: Java: Resolves default profile from config, reorders profiles by preference, looks up profile by id/name with case-insensitive matching. Rust has no equivalent utility; profile selection is implicit and UI-driven. No stored default profile ID in global settings.
- Verifikation: PARTIAL: Default profile ID is stored in GlobalSettings, but no helper service exists to resolve it with fallback logic, perform case-insensitive name lookups, or reorder profiles. The UI and backend rely on direct profile ID references without utility support for default resolution or flexible lookup.

### [MISSING] [medium] Enhanced AI prompt builders with hidden-reasoning rejection and hardening
- Java: src/main/java/de/kortty/core/AiPromptBuilder.java (commit 9809cf8 and subsequent), added hidden-reasoning rejection to CORRECT_SNIPPET_DESCRIPTION, CORRECT_SNIPPET_SELECTION_TEXT, TRANSLATE_SNIPPET_SELECTION_TEXT, DESCRIBE_SNIPPET_*, GENERATE_SNIPPET_* prompts
- Lücke: Java: Refined system prompts to reject <think> tags, hidden reasoning, invented facts, unsupported output formats across 8+ snippet AI actions. Rust has basic prompt builders in ai::mod.rs (build_system_prompt, build_user_prompt) for terminal AI only (Summarize, SolveProblem, Ask, GenerateChatTitle); no snippet-specific prompts or hardening language present.
- Verifikation: Rust prompt builders are basic and generic. Only cover terminal actions (Summarize, SolveProblem, Ask, GenerateChatTitle). No snippet-specific action prompts. No hardening language rejecting hidden reasoning or <think> tags. No snippet AI actions implemented at all (see next claim).

### [MISSING] [high] Snippet AI actions: COMPLETE, REVIEW, IMPROVE, ASSIST, SECURITY_REVIEW, APPLY_FIXES, ONE_LINER, PLANTUML
- Java: src/main/java/de/kortty/core/AiAction.java (enum extended); AiPromptBuilder (8 new action branches with JSON schema specifications); SnippetAiWorkflowSupport (applies workflows)
- Lücke: Java: Added 8 new snippet editor AI actions with detailed system prompts defining JSON response schemas (insertText, summary, findings, replacement, etc.). Rust AiAction enum has only 4 terminal-scoped actions (Summarize, SolveProblem, Ask, GenerateChatTitle); no snippet AI actions or workflows.
- Verifikation: Rust AiAction enum is terminal-only. No snippet editor AI actions, no JSON response schema definitions, no snippet-specific prompt builders, no SnippetAiWorkflowSupport equivalent.

### [MISSING] [high] AiConnectionMode field in ServerConnection: per-connection AI profile + skill assignment
- Java: src/main/java/de/kortty/model/ServerConnection.java (commit 830b949), fields aiProfileId and aiSkillIds with getters/setters; ConnectionEditDialog supports AI profile and skill selection
- Lücke: Java: Each SSH connection can be assigned a fixed AI profile (overriding default) and a list of CONNECTION-target AI skills. Rust ServerConnection model has no aiProfileId or aiSkillIds fields; no per-connection AI profile pinning or skill assignment UI.
- Verifikation: Rust ConnectionSettings has no per-connection AI profile pinning or skill assignment. No fields for overriding the default AI profile per SSH connection.

### [MISSING] [high] AiSkillTarget.CONNECTION: connection-scoped AI skills
- Java: src/main/java/de/kortty/model/AiSkillTarget.java (enum extended with CONNECTION value), requiresConnectionAssignment() method; AiSkillPromptSupport filters by CONNECTION target and assigned connection ids
- Lücke: Java: New CONNECTION target for skills; these skills only apply when assigned to the active connection via ServerConnection.aiSkillIds. AiSkillPromptSupport.fromSettings(settings, assignedConnectionSkillIds) filters skills by connection assignment and pins assigned skills to bypass relevance auto-detection. Rust AiSkillTarget has only Chat, Agent, Both; no CONNECTION target or connection-scoped filtering.
- Verifikation: Rust AiSkillTarget does not include CONNECTION target. No connection-scoped skills or connection assignment filtering logic.

### [MISSING] [medium] Pinned skills in relevance selection: assigned skills bypass auto-detection
- Java: src/main/java/de/kortty/core/AiSkillPromptSupport.java (constructor with pinnedSkillIds), AiSkillRelevanceSelector (withPinnedSkills method preserves pinned skills in selection)
- Lücke: Java: Skills assigned to a connection are pinned—they always remain in the selection even if auto-relevance detection would exclude them. Connection-assigned skills are passed as pinnedSkillIds to bypass relevance filtering. Rust ai_skills.rs has no pinning mechanism; all skill selection is based on learned relevance only.
- Verifikation: No skill pinning mechanism exists. All skills are selected via learned relevance only. No way to permanently include skills assigned to a connection.

### [MISSING] [low] AI skill target filtering by request context and request.includeAiSkills()
- Java: src/main/java/de/kortty/core/AiSkillPromptSupport.java (includeChatSkills method checks request.includeAiSkills())
- Lücke: Java: AiRequest can carry an includeAiSkills flag to signal whether skills should be appended. AiSkillPromptSupport checks this flag and skips skill appending if false. Rust AiRequestPayload has no includeAiSkills field; skill appending is unconditional.
- Verifikation: AiRequestPayload has no flag to signal whether skills should be appended. Skill appending is always enabled, with no way to disable it per-request.

### [MISSING] [high] Improved AI Manager dialog with CLI provider selection, model refresh, reasoning discovery
- Java: src/main/java/de/kortty/ui/AiManagerDialog.java (commit 830b949 enlarged to 500+ lines), new UI controls for connection mode, CLI provider dropdown, executable field, arguments textarea, model dropdown with refresh button, reasoning discovery button, CLI status display
- Lücke: Java: AiManagerDialog now has tabs/sections for HTTP API mode (url, api key, model, reasoning effort, internet access) and LOCAL_CLI mode (provider, executable, arguments template, with discovery/refresh actions). Also added default profile selector at top. Rust has AiManagerDialog.tsx but limited to HTTP mode; no CLI provider controls, no reasoning discovery UI, no connection mode switching.
- Verifikation: No connection mode tabs/sections. No CLI provider dropdown, executable path field, argument template textarea, or reasoning discovery button. No default profile selector at top of dialog. UI is HTTP API only.

### [PARTIAL] [medium] Default AI profile selection and persistence in Global Settings
- Java: src/main/java/de/kortty/core/GlobalSettingsManager.java, src/main/java/de/kortty/model/GlobalSettings.java (defaultAiProfileId field added in commit 830b949); AiManagerDialog persists selection
- Lücke: Java: Global settings now store defaultAiProfileId; AiProfileSelectionSupport resolves it to fallback if not found. AiManagerDialog UI selector at top of AI profiles tab. Rust GlobalSettings model has no defaultAiProfileId or default profile concept; profile selection is implicit.
- Verifikation: PARTIAL: The model field exists for persistence, but no UI control in AiManagerDialog or elsewhere allows users to set a default profile. The default_ai_profile_id is defined but not exposed to users or used in profile selection logic.

### [MISSING] [medium] Terminal agent decision normalization for robustness
- Java: src/main/java/de/kortty/core/TerminalAgentService.java (commit 830b949), normalizeAgentDecisionObject, normalizeAgentDecisionStatus, normalizeAgentDecisionCommands methods added; aliases status values and commands
- Lücke: Java: parseAgentDecision now normalizes JSON object to handle alternative field names and status values (run_commands vs run, needs_confirmation vs confirm, done vs complete/success, blocked vs cannot/failed). Normalizes command fields (command/cmd/shellCommand) and purpose/reason/description. Rust terminal_agent.rs parses AgentDecision but has no normalization logic for robustness against LLM output variations.
- Verifikation: Rust parse_agent_decision() does strict JSON deserialization with no field name normalization. No handling for LLM output variations (e.g., alternative command field names like 'cmd', 'shellCommand', 'command'; purpose/reason/description aliases). Fragile against model output variations.

### [MISSING] [medium] Terminal agent confirm-mutating-commands mode and approval logic refinement
- Java: src/main/java/de/kortty/core/TerminalAgentService.java (commit 830b949), confirmMutatingCommandSets vs autoApproveRootCommands fields in Request, Approval.allowAlways() method
- Lücke: Java: Terminal agent request now distinguishes confirmMutatingCommandSets (always require approval) from autoApproveRootCommands (auto-approve if allowed). Approval tracking stores allowAlways to prevent repeated re-approval prompts. Rust has MAX_AGENT_TURNS (8) and basic approval flow but lacks explicit confirmMutatingCommandSets mode or approval bypass refresh logic.
- Verifikation: PARTIAL: Rust has ask_confirmation_before_every_command and auto_approve_root_commands (TerminalAgentRequest, lines 35-36), but no distinct confirmMutatingCommandSets mode. The approval_bypass_enabled flag exists (TerminalAgentControl, line 59) to bypass future prompts, but no structured 'allowAlways' tracking or per-command approval caching.

### [MISSING] [medium] AI profile reasoning effort discovery and availability caching
- Java: src/main/java/de/kortty/model/AiProfile.java (discoveredReasoningEfforts and reasoningDiscoveryKey fields), populated by AiReasoningDiscoveryService
- Lücke: Java: AiProfile now stores discoveredReasoningEfforts list (result of discovery probe) and reasoningDiscoveryKey (cache key to avoid re-probing). AiManagerDialog shows discovered efforts in dropdown. Rust has reasoning_effort field but no discovery caching or cache key.
- Verifikation: Rust AiProfile has only reasoning_effort field. No discovery caching, no cache keys, no list of discovered/accepted reasoning efforts from API probing.

### [PARTIAL] [low] LM Studio native chat URL normalization to /v1/chat/completions
- Java: src/main/java/de/kortty/core/LocalLmModelResolver.java (commit 70e95b9), isLocalLmStudioBaseUrl, normalizeUrl logic; OpenAiCompatibleAiService normalizes /v1 endpoints
- Lücke: Java: LocalLmModelResolver detects LM Studio endpoints and normalizes /v1 base URLs to /v1/chat/completions for compatibility. Rust has normalize_lm_studio_chat_url() in ai::mod.rs but auto-normalization is limited—relies on user-configured url to include /v1/chat/completions.
- Verifikation: PARTIAL: Rust correctly normalizes LM Studio endpoints to /api/v1/chat. However, the normalization is automatic during dispatch only if the API is detected as LM Studio. If a user manually enters a /v1 URL without specifying LM Studio mode explicitly, it will be normalized to /v1/chat/completions (OpenAI style) rather than LM Studio style. User must be aware of the endpoint type.

### [MISSING] [medium] Job scheduler AI agent support with auto-approval
- Java: src/main/java/de/kortty/jobscheduler/JobSchedulerAiSupport.java (commit 830b949), new file with agent request configuration for scheduler; AI_AGENT job action
- Lücke: Java: New JobSchedulerAiSupport bridges AI agent into background job scheduler. Supports AI_AGENT scheduler action type with auto-approval and result persistence. Rust has terminal_agent.rs but no integration with jobscheduler.rs for background agent runs.
- Verifikation: Job scheduler handles snippets and other actions but does not integrate terminal agent. No background AI agent execution or auto-approval for scheduled jobs.

### [PARTIAL] [low] AI response sanitizer and response format handling
- Java: src/main/java/de/kortty/core/AiResponseSanitizer.java (referenced in various service implementations); supports response validation and format fallbacks
- Lücke: Java: Response sanitizer validates and cleans AI output. Rust has is_unsupported_json_response_format_error() for fallback handling (retries without JSON mode) but no general sanitization layer.
- Verifikation: PARTIAL: Rust implements fallback handling for unsupported JSON response formats (retries without JSON mode), but no general sanitization layer for response validation, format normalization, or cleaning.

## Bereich: update-checker (14 Lücken)

### [MISSING] [high] Update check service with automatic and manual triggers
- Java: src/main/java/de/kortty/update/UpdateCheckService.java; KorTTYApplication.java startUpdateCheckService()
- Lücke: Java implementation provides UpdateCheckService that runs automatic checks at startup and periodic 1-hour intervals, respects enable/disable toggle, and supports manual trigger. Service manages state like last-check timestamps, snooze/ignore lists, and throttling (7-day suppression for duplicate automatic prompts). No equivalent Rust backend service or frontend invocation exists.
- Verifikation: No UpdateCheckService equivalent exists in the Rust codebase. No Rust backend module for update checking, no command handlers for manual/automatic update checks. The frontend has no UI components or state management for triggering update checks. Unlike Java's startUpdateCheckService() invocation at startup, there is no initialization of any update checker service in Rust lib.rs.

### [MISSING] [high] GitHub API integration for fetching latest release
- Java: src/main/java/de/kortty/update/GitHubReleaseClient.java
- Lücke: GitHubReleaseClient fetches from https://api.github.com/repos/chardonnay/korTTY/releases/latest with custom headers, parses JSON response to extract tag_name, release name, assets list, draft/prerelease flags, and publish timestamp. No Rust HTTP client or GitHub API handler for update checks exists.
- Verifikation: No GitHubReleaseClient or equivalent HTTP client for GitHub API integration exists. The codebase has no code that fetches from api.github.com/repos/chardonnay/korTTY/releases/latest. While reqwest is in Cargo.toml for HTTP requests, it is used for other purposes (translation, AI APIs). No GitHub release asset parsing logic exists.

### [MISSING] [high] Semantic version parsing and comparison
- Java: src/main/java/de/kortty/update/UpdateVersion.java
- Lücke: UpdateVersion parses semver strings (v1.2.3-alpha format) and compares versions using major/minor/patch integers plus prerelease string comparison. Handles missing minor/patch segments and filters draft/prerelease releases as non-stable. No equivalent Rust version parser or comparison logic exists.
- Verifikation: No UpdateVersion equivalent exists. No semver parsing or version comparison logic for detecting newer releases. The Rust codebase does not import or use any semantic versioning library. No version comparison logic for major/minor/patch integers or prerelease filtering.

### [MISSING] [high] Platform-aware asset selection (Windows/macOS/Linux with architecture matching)
- Java: src/main/java/de/kortty/update/UpdateAssetSelector.java; PlatformProfile.java
- Lücke: UpdateAssetSelector filters GitHub release assets by platform (Windows .msi/.zip, macOS .dmg/.zip, Linux .deb/.rpm/.pkg.tar.zst/.tar.gz) and architecture (x86_64/amd64/x64, aarch64/arm64). PlatformProfile reads /etc/os-release on Linux to detect distro and prefer deb/rpm/arch packages. Falls back to generic java-zip if no native asset found. No Rust implementation of asset filtering or OS detection for updates exists.
- Verifikation: No UpdateAssetSelector or PlatformProfile equivalent. No logic to filter GitHub release assets by platform (.msi, .dmg, .deb, .rpm, etc.) or architecture (x86_64, aarch64). No /etc/os-release reading for Linux distro detection. No fallback mechanism for asset selection.

### [MISSING] [high] Atomic SHA256-verified asset download with progress
- Java: src/main/java/de/kortty/update/UpdateAssetDownloader.java
- Lücke: UpdateAssetDownloader fetches asset by URI over HTTP with 10s connect/10min request timeout, verifies SHA256 digest from release metadata, uses atomic file moves (.part → final), generates unique filenames if target exists, and reports download failures. No Rust download pipeline for update assets exists.
- Verifikation: No UpdateAssetDownloader equivalent. No HTTP asset downloading with SHA256 verification, atomic file operations (.part → final), timeout handling, or download progress reporting. The frontend has no download progress dialog or file operations for update assets.

### [MISSING] [high] Settings UI controls for update checks (enable/disable + interval slider)
- Java: src/main/java/de/kortty/ui/SettingsDialog.java (updateChecksEnabledCheck, updateCheckIntervalSlider, updateCheckIntervalValueLabel)
- Lücke: SettingsDialog exposes CheckBox and Slider for updateChecksEnabled boolean and updateCheckIntervalDays (1-30 days, default 7). Rust SettingsDialog.tsx has no update-check controls or GlobalSettings fields for updateChecksEnabled, updateCheckIntervalDays, or related state.
- Verifikation: The SettingsDialog.tsx has no update-check controls. GlobalSettings interface and model have no updateChecksEnabled, updateCheckIntervalDays, or related fields. The settings tabs in SettingsDialog.tsx (language, translation, ai, backup, window, terminal) contain no update checker section. Rust GlobalSettings struct at /Users/daniel/Software-Projects/kortty_rust/korTTY_rust/src-tauri/src/model/settings.rs has no update-related fields.

### [MISSING] [high] Automatic update notification dialog with user action buttons
- Java: src/main/java/de/kortty/ui/MainWindow.java showUpdateAvailableDialog()
- Lücke: When automatic check finds an update, MainWindow displays a dialog with version label, asset name, and three buttons: Download (starts download), Remind Tomorrow (snooze until next day), Skip Version (ignore forever). Download and snooze state are persisted. No equivalent dialog component or user-response handling in Rust.
- Verifikation: No UpdateAvailableDialog or equivalent component exists. The frontend dialog list (AiActionDialog, AiAgentDialog, BackupDialog, SettingsDialog, etc.) contains no update notification dialog. No dialog state for presenting version availability with Download/Remind Tomorrow/Skip Version buttons. No event handling for update notification display.

### [MISSING] [medium] Manual update check with progress UI (button, spinner, status label)
- Java: src/main/java/de/kortty/ui/MainWindow.java runManualUpdateCheck()
- Lücke: MainWindow displays manual check button in settings-adjacent UI, shows spinner and status label during check, disables button during check, reports success (available/up-to-date), asset-not-found, or failure with error detail. No Rust UI or command for manual update checks exists.
- Verifikation: No manual update check UI in settings or anywhere else. SettingsDialog.tsx has no update check button, spinner, or status label. No command handler for manual update checks in the Rust backend.

### [MISSING] [medium] Download with progress dialog and local file link
- Java: src/main/java/de/kortty/ui/MainWindow.java downloadUpdate()
- Lücke: When user clicks Download button, a progress dialog shows asset name, running status, and (on success) opens the Downloads directory in file browser or shows error. File is downloaded to OS downloads directory (resolved by DownloadDirectoryResolver). No Rust download dialog or file-browser integration exists.
- Verifikation: No download progress dialog component exists. The frontend has no UI for showing download progress or opening the Downloads directory after download completion. No integration with OS file browser (Finder, Explorer, etc.) for downloaded updates.

### [MISSING] [medium] Automatic check throttling (7-day suppression for same version, 1-day startup suppression)
- Java: src/main/java/de/kortty/update/UpdateCheckService.java shouldSuppressAutomaticPrompt()
- Lücke: Automatic checks suppress prompts if user has already seen the same version within 7 days (periodic) or same day (startup). Ignored versions suppress forever; snoozed versions suppress until the set date. Persisted in lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate, snoozedUpdateVersion, updateSnoozedUntilLocalDate. No Rust state tracking or suppression logic exists.
- Verifikation: No throttling or suppression logic for update checks. GlobalSettings has no lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate, snoozedUpdateVersion, or updateSnoozedUntilLocalDate fields. No shouldSuppressAutomaticPrompt() logic.

### [MISSING] [high] Global settings persistence for update state
- Java: src/main/java/de/kortty/model/GlobalSettings.java
- Lücke: GlobalSettings stores updateChecksEnabled, updateCheckIntervalDays (1-30, default 7), lastSuccessfulUpdateCheckMillis, ignoredUpdateVersion, snoozedUpdateVersion, updateSnoozedUntilLocalDate, lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate. Rust GlobalSettings interface and model have no equivalent fields.
- Verifikation: GlobalSettings model completely lacks update-related persistence fields: updateChecksEnabled, updateCheckIntervalDays, lastSuccessfulUpdateCheckMillis, ignoredUpdateVersion, snoozedUpdateVersion, updateSnoozedUntilLocalDate, lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate. These fields are not in either the Rust model or TypeScript interface.

### [MISSING] [high] Periodic background check (1-hour interval, respects check-interval setting)
- Java: src/main/java/de/kortty/update/UpdateCheckService.java start() and scheduleWithFixedDelay()
- Lücke: After startup automatic check, UpdateCheckService schedules periodic checks every 1 hour, but only notifies user if isAutomaticCheckDue() returns true (respects updateCheckIntervalDays from settings). Can be stopped/restarted when settings change. No Rust background scheduler or periodic check mechanism exists.
- Verifikation: No background scheduler for update checks exists. Unlike Java's UpdateCheckService.scheduleWithFixedDelay(), the Rust lib.rs initialization has no setup for periodic update checks. No tokio::spawn or scheduled task for hourly update checks. The Rust codebase does initialize a JobSchedulerManager for job scheduling, but this is for user-defined jobs, not built-in update checks.

### [MISSING] [low] Error handling and logging for update checks (InterruptedException, IOException, JSON parse failures)
- Java: src/main/java/de/kortty/update/UpdateCheckService.java checkForUpdate(); GitHubReleaseClient.java
- Lücke: Robust error handling for network timeouts, HTTP errors, JSON parse failures, invalid version strings, missing assets, and interrupted tasks. Failures logged at debug level; user-facing error messages returned in UpdateCheckResult. No Rust error handling for update operations exists.
- Verifikation: No error handling logic exists for update operations since no update service is implemented. No error types or result wrappers for network timeouts, HTTP errors, JSON parse failures, or invalid version strings. No logging infrastructure for update check failures.

### [MISSING] [medium] Data classes for release/asset/version/check-result (immutable records)
- Java: UpdateRelease.java, UpdateAsset.java, AvailableUpdate.java, UpdateCheckResult.java, UpdateCheckRunType.java
- Lücke: Java uses records for UpdateRelease (tagName, name, htmlUri, publishedAt, draft, prerelease, assets), UpdateAsset (name, downloadUri, size, digest), AvailableUpdate (release, asset, latestVersion, currentVersion), and enums UpdateCheckRunType, UpdateCheckResult.Status. Rust lacks all corresponding types.
- Verifikation: No data types equivalent to Java's UpdateRelease, UpdateAsset, AvailableUpdate, UpdateCheckResult, or UpdateCheckRunType. The Rust codebase has no structs or TypeScript interfaces for representing release information, assets, or check results. No enums for check run type.

## Bereich: design-ux (18 Lücken)

### [MISSING] [medium] App-level visual designs (enum with previews)
- Java: 81b61d2 commit: AppDesign.java (5 designs: NORMAL, MATRIX_TERMINAL, HOLOGRAPHIC_INTERFACE, KLINGON_TACTICAL, ELEGANT_DARK), preview PNG images in src/main/resources/previews/
- Rust-Stand: src-tauri/src/commands/gui_theme_commands.rs (no app-design themes), src/store/guiThemeStore.ts (lacks appDesign field)
- Lücke: Java implements AppDesign enum with stable ID persistence and GUI theme selection via AppDesignStyleSupport. Rust has guiThemeStore with 16 builtin color-palette themes (Catppuccin, Dracula, Nord, etc.) but lacks the app-design concept. The Java designs style JavaFX dialogs/windows/menus/buttons; Rust GUI themes style CSS root variables. No enum mapping for 'matrix-terminal', 'holographic-interface', 'klingon-tactical', 'elegant-dark' in Rust backend or frontend.
- Verifikation: The Java app has AppDesign.java enum with 5 designs (NORMAL, MATRIX_TERMINAL, HOLOGRAPHIC_INTERFACE, KLINGON_TACTICAL, ELEGANT_DARK) with stable ID persistence. The Rust app has guiThemeStore.ts with 16 builtin color-palette themes (Catppuccin variants, Dracula, Nord, etc.) but no app-design enum mapping. GlobalSettings in both TypeScript and Rust backend lack appDesign/appDesignId field. No concept of app-level designs separate from GUI themes exists.

### [PARTIAL] [low] AppDesign stylesheet application (JavaFX CSS injection)
- Java: 81b61d2: AppDesignStyleSupport.java with applyToStylesheets(), CSS files in src/main/resources/styles/ (matrix-terminal.css, holographic.css, tactical.css, elegant.css)
- Rust-Stand: src/store/guiThemeStore.ts applyGuiThemeToCss() function, src/index.css (Tailwind)
- Lücke: Java AppDesignStyleSupport dynamically applies design-specific CSS stylesheets to all JavaFX scenes and dialogs. Rust has guiThemeStore that applies theme colors to CSS variables via applyGuiThemeToCss() but does not manage app-design-specific stylesheets. Tailwind CSS is used for styling instead of design-specific CSS files.
- Verifikation: Java has AppDesignStyleSupport.java with applyToStylesheets() and design-specific CSS files (matrix-terminal.css, holographic.css, tactical.css, elegant.css). Rust has guiThemeStore with applyGuiThemeToCss() that applies theme colors to CSS root variables via setProperty(). The fundamental difference is architecture: Java uses app-design-specific stylesheets while Rust uses CSS variables. Rust's approach is more modern but lacks the concept of distinct app designs.

### [MISSING] [high] Design persistence in GlobalSettings (appDesign field)
- Java: 81b61d2: GlobalSettings.java with getAppDesign()/setAppDesign(), XML serialization
- Rust-Stand: src/store/settingsStore.ts GlobalSettings interface (lines 18-82) has no appDesign field; src-tauri/src/model/settings.rs also lacks this
- Lücke: Java GlobalSettings stores selected app design ID (default: NORMAL). Rust GlobalSettings (settingsStore.ts) lacks appDesign/appDesignId field entirely. User selection of matrix/holographic/tactical designs cannot be persisted.
- Verifikation: Java GlobalSettings.java has getAppDesign()/setAppDesign() with XML serialization. Rust GlobalSettings (TypeScript settingsStore.ts and Rust src-tauri/src/model/settings.rs) completely lacks appDesign or appDesignId field. No persistence mechanism exists for app designs in Rust.

### [MISSING] [high] Settings dialog Appearance tab for design selection
- Java: 81b61d2: SettingsDialog.java with appearance section showing design previews and radio selection
- Rust-Stand: src/components/dialogs/SettingsDialog.tsx line 22: TabId definition lacks appearance option
- Lücke: Java SettingsDialog includes an Appearance tab with selectable design previews and radio buttons. Rust SettingsDialog (src/components/dialogs/SettingsDialog.tsx) has TabId values of 'language'|'translation'|'ai'|'backup'|'window'|'terminal' but no 'appearance' tab.
- Verifikation: Java SettingsDialog has Appearance tab with design previews and radio selection. Rust SettingsDialog.tsx defines TabId as 'language'|'translation'|'ai'|'backup'|'window'|'terminal' (line 22), with tabs array at line 205-212 showing exactly these 6 tabs. No appearance/design tab exists.

### [PARTIAL] [medium] Ctrl+Q window close shortcut (secondary windows only)
- Java: e652da2 commit: WindowCloseShortcutSupport.java (Ctrl+Q closes secondary windows, primary main window excluded)
- Rust-Stand: src/components/MainWindow.tsx lines 3730, 3855 (onQuit handler exists but lacks primary/secondary window distinction)
- Lücke: Java provides Ctrl+Q shortcut only for secondary windows (not the primary main window) via WindowCloseShortcutSupport. Rust MainWindow.tsx implements Ctrl+Q (line 3730) but does not distinguish between primary and secondary windows; it triggers handleQuit() for all windows. Secondary window handling is incomplete.
- Verifikation: Java WindowCloseShortcutSupport.java implements Ctrl+Q that excludes the primary main window (line 107: 'if (window == null || window == primaryMainWindow) return'). Rust MainWindow.tsx line 3730 implements Ctrl+Q via 'if (ctrl && !shift && e.key === "q")' and calls handleQuit() for all windows without primary/secondary distinction. Secondary window handling exists in Rust via window labels (createAdditionalWindow at line 2366), but the Ctrl+Q shortcut applies globally instead of only to secondary windows.

### [PARTIAL] [low] Master password dialog visual refresh (black background, larger logo, colored input)
- Java: 81b61d2: MasterPasswordDialog.java with new layout (logo display, black bg, light text, blue password field)
- Rust-Stand: src/components/dialogs/MasterPasswordDialog.tsx (uses theme variables, not the specific Java styling)
- Lücke: Java MasterPasswordDialog was redesigned with black background, larger KorTTY logo, readable light text, and blue password field. Rust MasterPasswordDialog.tsx uses tailwind classes (bg-kortty-bg, text-kortty-text) which inherit from GUI theme but do not replicate the Java visual refresh. Logo display exists but styling differs.
- Verifikation: Java MasterPasswordDialog was redesigned with black background, larger logo, light text, blue password field. Rust MasterPasswordDialog.tsx uses Tailwind classes (bg-kortty-bg, text-kortty-text, input-field) which inherit from GUI theme. It shows KeyRound/LockKeyhole icons (lines 61-62) instead of a distinct logo image. The styling is GUI theme-aware but differs from Java's visual refresh: no distinct logo image display, colors depend on active theme (not hardcoded black+blue).

### [PARTIAL] [low] Dialog theming integration (theme-aware alerts and common dialogs)
- Java: 81b61d2: DialogThemeHelper.java integration in MasterPasswordDialog, ConnectionEditDialog, ConnectionManagerDialog, QuickConnectDialog, SettingsDialog for consistent design application
- Rust-Stand: Dialog components inherit guiThemeStore colors via CSS variables but lack app-design-specific behavior
- Lücke: Java ensures all dialogs respect the active AppDesign through DialogThemeHelper. Rust dialogs automatically use Tailwind theme variables but are not design-aware (no matrix/holographic/tactical/elegant styling variant selection per dialog).
- Verifikation: Java DialogThemeHelper integrates AppDesign into MasterPasswordDialog, ConnectionEditDialog, etc. Rust dialogs inherit GUI theme colors via Tailwind theme variables (kortty-bg, kortty-text, etc.) automatically. All dialogs in Rust are technically theme-aware through the CSS variable system, but they are not 'design-aware' in the Java sense (no app-design enum variant selection per dialog). The theming mechanism exists but operates at a lower level (CSS variables) rather than app-design logic.

### [MISSING] [low] ResizableDivider for file browser and panel resizing
- Java: 81b61d2: ResizableDivider.java (draggable 3px divider with H_RESIZE/V_RESIZE cursor, supports left/right and top/bottom layouts)
- Rust-Stand: src/components/files/LocalFileBrowser.tsx lacks ResizeListener pattern and drag event handlers
- Lücke: Java provides a ResizableDivider component for resizing adjacent panels. Rust LocalFileBrowser.tsx has inline div layout with width/height classes but no draggable resize handle implementation. File browser docking is static.
- Verifikation: Java has ResizableDivider.java class with draggable 3px divider supporting H_RESIZE/V_RESIZE cursors. Rust LocalFileBrowser.tsx (lines 85-91) uses static width/height Tailwind classes (w-[280px], h-56, border-t/l/r) without any draggable resize handle implementation. No ResizableDivider component exists in Rust.

### [PARTIAL] [medium] Local file browser docking (left/right/bottom sides)
- Java: 81b61d2: MainWindow.java with LocalFileBrowser and dockable positioning; LocalFileBrowser.java with layout logic
- Rust-Stand: src/components/MainWindow.tsx implements dock positioning without resize handles; src/components/files/LocalFileBrowser.tsx
- Lücke: Java MainWindow shows LocalFileBrowser on left, right, or bottom with dockable panels and resizable dividers. Rust MainWindow.tsx has localFileBrowserDock setting (left/right/bottom) with conditional rendering (lines 3964, 3975, 4249) but lacks resizable dividers.
- Verifikation: Java MainWindow shows LocalFileBrowser with dockable positioning and resizable dividers. Rust MainWindow.tsx implements localFileBrowserDock setting (left/right/bottom) at lines 3964, 3975, 4249 with conditional rendering. The setting exists (settingsStore.ts line 80) and is persisted. However, resizable dividers are missing—the browser is static-sized.

### [PARTIAL] [medium] Terminal-only fullscreen mode (F12 toggle)
- Java: 81b61d2: MainWindow.java with F12 hotkey and View menu option hiding menu bar, status bar, dashboard, file browser while keeping terminal visible
- Rust-Stand: src/components/MainWindow.tsx: terminalOnlyFullscreen state exists, F12 handler exists, conditional UI hiding present but restoration state tracking unclear
- Lücke: Java F12 toggles terminal-only fullscreen and hides menu/status/dashboard/file-browser chrome, restoring previous state on toggle off. Rust MainWindow.tsx implements F12 (line 3742) and terminalOnlyFullscreen state (line 675) with conditional hiding of menu/dashboard/file-browser (lines 3871, 3873, 3964, 3975, 4249) but restoration logic may be incomplete.
- Verifikation: Java F12 toggles terminal-only fullscreen hiding menu bar, status bar, dashboard, file browser. Rust MainWindow.tsx line 3742 handles F12 and line 675 has terminalOnlyFullscreen state. Conditional hiding at lines 3871, 3873, 3964, 3975, 4249 hides menu/dashboard/file-browser. The feature is implemented but the original claim's assertion about 'restoration logic may be incomplete' cannot be confirmed without deeper state analysis. The basic toggle and hiding mechanism is present.

### [MISSING] [low] Hide terminal scrollbars in fullscreen option
- Java: 81b61d2: MainWindow.java with View menu checkbox 'Hide terminal scrollbars in fullscreen' and corresponding setting
- Rust-Stand: SettingsDialog.tsx lacks this option; MainWindow.tsx lacks hideFullscreenScrollbars state
- Lücke: Java provides a separate checkbox to hide terminal scrollbars only during terminal-only fullscreen mode. Rust does not have this setting or its UI option.
- Verifikation: Java has a dedicated View menu checkbox 'Hide terminal scrollbars in fullscreen' with corresponding setting. Rust has no equivalent: no hideTerminalScrollbarsInFullscreen setting in settingsStore.ts or in the Rust backend model, and no UI option in SettingsDialog.

### [MISSING] [medium] Tools menu shortcuts for AI and scheduler actions (Cmd/Ctrl+Shift+E, J, V, A, P)
- Java: eca75ba commit: MainWindow.java with keyboard shortcuts: Ctrl+Shift+E (recording toggle), Ctrl+Shift+J (job scheduler), Ctrl+Shift+V (video manager), Ctrl+Alt+A (AI agent), Ctrl+Alt+P (AI planning)
- Rust-Stand: src/hooks/useKeyboard.ts (lines 58-76) lacks Shift+E, Shift+J, Shift+V, Alt+A, Alt+P handlers
- Lücke: Java MainWindow defines multiple keyboard accelerators for tools menu actions (recording, job scheduler, video manager, AI agent, AI planning). Rust MainWindow.tsx does not implement these specific keyboard shortcuts. Custom shortcuts may be planned but not yet wired.
- Verifikation: Java MainWindow.java (eca75ba commit) defines keyboard shortcuts: Ctrl+Shift+E (recording toggle), Ctrl+Shift+J (job scheduler), Ctrl+Shift+V (video manager), Ctrl+Alt+A (AI agent), Ctrl+Alt+P (AI planning). Rust MainWindow.tsx implements Ctrl+Shift+Y for AI Manager (line 3729) and no keyboard shortcuts for recording, job scheduler, video manager, or planning. Only Ctrl+Shift+Y is wired among the tools.

### [PARTIAL] [low] Startup refactor and app architecture changes
- Java: 62a87d0 commit: KorTTYApplication.java with LoggingConfiguration bootstrap, applyLoggingSettings(), startUpdateCheckService(), log maintenance executor, UpdateCheckService initialization
- Rust-Stand: src-tauri/src/lib.rs run() function (lines 23-187) lacks LoggingConfiguration and UpdateCheckService initialization compared to Java
- Lücke: Java implements comprehensive startup refactoring: logging configuration initialization (LoggingConfiguration.bootstrapFromPersistedSettings), log maintenance thread, and UpdateCheckService start on app launch. Rust src-tauri/src/lib.rs initializes managers and plugins but does not expose equivalent logging configuration or update check service startup.
- Verifikation: Java KorTTYApplication.java (62a87d0 commit) implements LoggingConfiguration bootstrap with applyLoggingSettings(), log maintenance executor, and UpdateCheckService initialization on startup. Rust src-tauri/src/lib.rs initializes managers (SSHManager, JobSchedulerManager, TerminalAgentStore, etc.) and plugins at lines 33-48, but does not expose logging configuration initialization (bootstrapFromPersistedSettings) or update check service. The Rust architecture initializes components but lacks the explicit logging configuration and update check startup patterns from Java.

### [PARTIAL] [low] KorTTY logo refresh in login and about dialogs
- Java: 81b61d2: KorTTY logo assets refreshed (kortty_logo.png), displayed in MasterPasswordDialog and About dialog
- Rust-Stand: src/components/dialogs/MasterPasswordDialog.tsx uses lucide-react icons instead of custom logo image
- Lücke: Java refreshed logo assets (macOS, Windows, Linux icons and in-app logo). Rust may have icon resources but logo display in dialogs (MasterPasswordDialog, about) is not explicitly featured. MasterPasswordDialog.tsx shows icons but no distinct logo image.
- Verifikation: Java refreshed logo assets (kortty_logo.png) displayed in MasterPasswordDialog and About dialog. Rust MasterPasswordDialog.tsx shows KeyRound/LockKeyhole Lucide icons (lines 61-62) instead of a distinct logo image. The About dialog (MainWindow.tsx lines 4523-4555) displays version text and GitHub link but no distinct logo image—only text 'KorTTY' in large font (line 4527). Logo display is not explicitly featured; icons and text are used instead.

### [MISSING] [medium] Terminal color sequence control setting (disable ANSI/TrueColor per connection)
- Java: 81b61d2: TerminalColorControlSequenceFilter.java, Quick Connect and Connection Edit dialogs with color disable option
- Rust-Stand: src/components/dialogs/QuickConnect.tsx and ConnectionEditor.tsx lack color control options
- Lücke: Java allows disabling terminal colors globally or per-connection through Settings and Connection UI. Rust does not expose this setting in QuickConnect or ConnectionEditor components.
- Verifikation: Java has TerminalColorControlSequenceFilter.java with settings in Quick Connect and Connection Edit dialogs to disable terminal colors globally or per-connection. Rust QuickConnect.tsx and ConnectionEditor.tsx have no equivalent setting for disabling ANSI/TrueColor sequences. No 'disableTerminalColors' or similar field exists in ConnectionSettings (Rust backend connection.rs lines 17-70).

### [MISSING] [medium] Per-connection terminal emulation selection (TERM value)
- Java: 81b61d2: Quick Connect and Connection Edit store selected terminal emulation for SSH/Mosh sessions
- Rust-Stand: src/components/dialogs/QuickConnect.tsx and ConnectionEditor.tsx lack terminal emulation selector
- Lücke: Java Quick Connect and Connection Edit allow selecting terminal emulation (e.g., xterm-256color). Rust equivalents do not expose this setting.
- Verifikation: Java Quick Connect and Connection Edit allow selecting terminal emulation (e.g., xterm-256color) with TerminalEmulationComboBoxSupport. Rust ConnectionSettings struct (src-tauri/src/model/connection.rs lines 17-70) lacks any TERM or terminal emulation field. QuickConnect.tsx and ConnectionEditor.tsx have no UI for this setting.

### [PARTIAL] [low] Keyboard shortcut for Snippet Manager and other dialogs
- Java: eca75ba: MainWindow.java with keyboard shortcut handlers for snippet manager dialog open
- Rust-Stand: src/hooks/useKeyboard.ts has basic keyboard handlers; src/components/MainWindow.tsx manages dialog state but shortcut-to-dialog mapping unclear
- Lücke: Java MainWindow maps keyboard shortcuts to open various dialogs including Snippet Manager. Rust does implement keyboard handling but specific dialog-open shortcuts mapping may be incomplete.
- Verifikation: Java MainWindow.java maps keyboard shortcuts to open various dialogs including Snippet Manager. Rust MainWindow.tsx implements keyboard handlers (lines 3697-3782) including Ctrl+T (new tab), Ctrl+Shift+D (dashboard), Ctrl+K (quick connect), Ctrl+Shift+Y (AI manager), etc. Snippet Manager open action is handled via MenuBar onSnippets callback (line 3839-3841) but no keyboard shortcut is bound. Other dialogs have keyboard shortcuts, but Snippet Manager does not.

### [MISSING] [low] Update check service and automatic update notification
- Java: eca75ba: UpdateCheckService.java, UpdateCheckRunType, UpdateVersion parsing, automatic check on startup, user notification
- Rust-Stand: No update check commands in src-tauri/src/lib.rs invoke_handler; no update UI in MainWindow
- Lücke: Java implements UpdateCheckService that checks GitHub releases on startup and shows automatic update notifications. Rust backend and frontend do not expose update checking functionality.
- Verifikation: Java has UpdateCheckService.java that checks GitHub releases on startup and shows automatic update notifications. Rust backend (src-tauri/src/lib.rs) has no UpdateCheckService equivalent. Tauri config (tauri.conf.json) has no 'updater' section configured. The About dialog (MainWindow.tsx lines 4523-4555) shows only the static version 2.2.0 with no update check UI. No update checking functionality is implemented in Rust.

## Bereich: core-misc (19 Lücken)

### [MISSING] [high] LoggingConfiguration bootstrap and maintenance
- Java: src/main/java/de/kortty/core/LoggingConfiguration.java (new file, v2.2.0); logback.xml updated with configurable log directory
- Rust-Stand: src-tauri/src/logging.rs only implements TerminalLogger for session output, not global logging configuration.
- Lücke: Java v2.2.0 introduced LoggingConfiguration class for bootstrapping logging from persisted settings, maintaining log directories with retention-based cleanup, compression of old logs (>24h) to .gz, and Logback reconfiguration at runtime. Supports customizable log directory (~-expansion, relative/absolute paths), retention days (0-3650, default 7), automatic cleanup, and log rotation. Rust has basic TerminalLogger for gzip compression of session logs but no centralized logging configuration management, retention policies, or log directory persistence.
- Verifikation: Rust repo has basic TerminalLogger in src-tauri/src/logging.rs that compresses session logs to .gz files in a history directory, but lacks centralized logging configuration management, log retention policies (0-3650 days), automatic cleanup, log directory persistence from settings, and runtime Logback reconfiguration. Java implements LoggingConfiguration class for bootstrapping logging from GlobalSettings with dynamic log directory management and retention-based cleanup.

### [MISSING] [high] GlobalSettings: Log directory path and retention configuration
- Java: src/main/java/de/kortty/model/GlobalSettings.java: logDirectoryPath field, logRetentionDays (int, 0-3650, default 7), with getters/setters and normalization
- Rust-Stand: src-tauri/src/model/settings.rs: no log_directory_path or log_retention_days fields found.
- Lücke: Java GlobalSettings added logDirectoryPath (null/blank = ~/.kortty/logs, supports ~ expansion) and logRetentionDays (clamped 0-3650, default 7). Rust GlobalSettings model does not include these fields. This is required to persist user-chosen log directory and retention policy across app restarts.
- Verifikation: Rust GlobalSettings (src-tauri/src/model/settings.rs) does not have logDirectoryPath or logRetentionDays fields. Java GlobalSettings has both with normalization and defaults (7 days, ~-expansion support, 0-3650 range). These are required to persist user-chosen log directory and retention policy.

### [MISSING] [medium] GlobalSettings: Update check configuration
- Java: src/main/java/de/kortty/model/GlobalSettings.java: updateChecksEnabled (bool, default true), updateCheckIntervalDays (1-30, default 1), lastSuccessfulUpdateCheckMillis, ignoredUpdateVersion, snoozedUpdateVersion, updateSnoozedUntilLocalDate, lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate
- Rust-Stand: src-tauri/src/model/settings.rs: no update-related fields present.
- Lücke: Java v2.2.0 adds configurable automatic update checking with snooze/ignore capabilities, tracking last check time and prompt history. Rust does not have update checking infrastructure or settings for controlling update behavior.
- Verifikation: Rust GlobalSettings lacks all update check fields: updateChecksEnabled, updateCheckIntervalDays (1-30), lastSuccessfulUpdateCheckMillis, ignoredUpdateVersion, snoozedUpdateVersion, updateSnoozedUntilLocalDate, lastAutomaticUpdatePromptVersion, lastAutomaticUpdatePromptLocalDate. Java implements full automatic update checking with snooze/ignore capabilities and prompt history tracking.

### [PARTIAL] [high] GlobalSettings: Terminal recording configuration
- Java: src/main/java/de/kortty/model/GlobalSettings.java: terminalRecordingEnabled (bool), terminalRecordingStoragePath (null/blank = ~/.kortty/recordings), terminalRecordingFormat (enum: KORTTY_REPLAY, WEBM), terminalRecordingDefaultScope (enum: ACTIVE_SPLIT, WHOLE_TAB), terminalRecordingAutoPauseEnabled (bool, default true), terminalRecordingIdlePauseSeconds (1-3600, default 20), terminalRecordingFfmpegPath (null = ffmpeg from PATH), terminalRecordingCaptureColorsEnabled (bool, default false)
- Rust-Stand: src-tauri/src/model/settings.rs lines 166-174: terminal_recording_enabled, terminal_recording_idle_auto_pause, terminal_recording_directory; missing format/scope/ffmpeg/color fields.
- Lücke: Rust has terminal_recording_enabled and terminal_recording_idle_auto_pause and terminal_recording_directory. Missing: terminalRecordingFormat (KORTTY_REPLAY vs WEBM enum), terminalRecordingDefaultScope (ACTIVE_SPLIT vs WHOLE_TAB enum), terminalRecordingIdlePauseSeconds (concrete idle pause duration in seconds), terminalRecordingFfmpegPath (custom ffmpeg path), terminalRecordingCaptureColorsEnabled (color capture option). These control recording format, scope, auto-pause timing, and video export capabilities.
- Verifikation: Rust GlobalSettings has terminal_recording_enabled, terminal_recording_idle_auto_pause, and terminal_recording_directory. Missing: (1) terminalRecordingFormat enum (KORTTY_REPLAY vs WEBM/MKV - Rust has TerminalRecordingExportFormat in model/terminal_recording.rs but not in GlobalSettings); (2) terminalRecordingDefaultScope enum (ACTIVE_SPLIT vs WHOLE_TAB - enum exists in TerminalRecordingScope but not persisted in settings); (3) terminalRecordingIdlePauseSeconds (concrete idle pause duration in seconds); (4) terminalRecordingFfmpegPath (custom ffmpeg path); (5) terminalRecordingCaptureColorsEnabled (color capture option).

### [MISSING] [medium] GlobalSettings: Terminal UI appearance field (hideTerminalScrollbarsInFullscreen)
- Java: src/main/java/de/kortty/model/GlobalSettings.java: hideTerminalScrollbarsInFullscreen (bool, default false)
- Rust-Stand: src-tauri/src/model/settings.rs: no hideTerminalScrollbarsInFullscreen equivalent found.
- Lücke: Java v2.2.0 adds hideTerminalScrollbarsInFullscreen to allow hiding terminal scrollbars when in fullscreen mode. Related i18n key: 'menu.view.hideTerminalScrollbarsFullscreen'. Rust does not have this setting.
- Verifikation: Rust GlobalSettings does not have hideTerminalScrollbarsInFullscreen field. Java GlobalSettings has this boolean (default false) to allow hiding terminal scrollbars when in fullscreen mode. No i18n key 'menu.view.hideTerminalScrollbarsFullscreen' in Rust frontend.

### [MISSING] [medium] GlobalSettings: App design selector (appDesign enum)
- Java: src/main/java/de/kortty/model/AppDesign.java (new enum): NORMAL, MATRIX_TERMINAL, HOLOGRAPHIC_INTERFACE, KLINGON_TACTICAL, ELEGANT_DARK; GlobalSettings.appDesign field stores selected design ID
- Rust-Stand: src-tauri/src/model/settings.rs: no app_design field; src/components/dialogs/SettingsDialog.tsx has no 'appearance' tab (only language, translation, ai, backup, window, terminal tabs).
- Lücke: Java v2.2.0 introduces app-wide design selector with five built-in designs (Normal, Matrix Terminal, Holographic Interface, Klingon Tactical, Elegant Dark). GlobalSettings persists selected design. Rust has no AppDesign enum or app-design field in settings model. Frontend has no appearance settings tab with design selector.
- Verifikation: Rust GlobalSettings does not have appDesign field. Java AppDesign enum (5 designs: NORMAL, MATRIX_TERMINAL, HOLOGRAPHIC_INTERFACE, KLINGON_TACTICAL, ELEGANT_DARK) is persisted in GlobalSettings. Rust SettingsDialog has no appearance tab with design selector. i18n file checked (src/i18n/en.json) shows no 'settings.appearance.*' or 'settings.tab.appearance' keys.

### [PARTIAL] [medium] GlobalSettings: Snippet editor configuration
- Java: src/main/java/de/kortty/model/GlobalSettings.java: snippetDiagramBackgroundColor, selectedSnippetEditorProfileId, snippetEditorProfiles (list of SnippetEditorProfile), snippetManagerPreviewDividerPosition (Double), snippetHistoryMaxSize (Integer, default 30, max 99)
- Rust-Stand: src-tauri/src/model/settings.rs: no snippet-related fields; src-tauri/src/model/snippet.rs SnippetEditorProfile has no color fields (missing 15+ color properties from Java version).
- Lücke: Rust has snippet editor model but GlobalSettings lacks snippetDiagramBackgroundColor, selectedSnippetEditorProfileId selector, snippetEditorProfiles list persistence, snippetManagerPreviewDividerPosition, and snippetHistoryMaxSize. The Java SnippetEditorProfile has extensive color fields (foregroundColor, backgroundColor, cursorColor, commentColor, stringColor, numberColor, booleanColor, keyColor, keywordColor, sectionColor, variableColor, braceColor, cursorStyle) whereas Rust SnippetEditorProfile only has language, formatter_command, formatter_args, tab_size, insert_spaces (formatter-related, not color-related).
- Verifikation: Rust GlobalSettings completely lacks snippet editor configuration. Missing from both settings and SnippetEditorProfile: (1) snippetDiagramBackgroundColor; (2) selectedSnippetEditorProfileId selector; (3) snippetEditorProfiles list persistence in GlobalSettings (only in Snippet model); (4) snippetManagerPreviewDividerPosition; (5) snippetHistoryMaxSize (default 30, max 99). Rust SnippetEditorProfile (src-tauri/src/model/snippet.rs) has only formatter-related fields (formatter_command, formatter_args, tab_size, insert_spaces), completely lacking Java's color fields (foregroundColor, backgroundColor, cursorColor, commentColor, stringColor, numberColor, booleanColor, keyColor, keywordColor, sectionColor, variableColor, braceColor, cursorStyle).

### [MISSING] [high] GlobalSettings: Terminal agent execution control
- Java: src/main/java/de/kortty/model/GlobalSettings.java: terminalAgentExecutionEnabled (bool, default true), terminalAgentConfirmMutatingCommandSets (bool, default false)
- Rust-Stand: src-tauri/src/model/settings.rs: no terminal_agent_execution_enabled or terminal_agent_confirm_mutating_command_sets fields.
- Lücke: Java v2.2.0 adds two new boolean fields to control AI agent safety: terminalAgentExecutionEnabled (disables executable agent runs while keeping AI planning/ask available) and terminalAgentConfirmMutatingCommandSets (requires confirmation for mutating operations). Rust does not have these safety-gate fields in settings.
- Verifikation: Rust GlobalSettings does not have terminalAgentExecutionEnabled or terminalAgentConfirmMutatingCommandSets fields. Java v2.2.0 adds these two boolean fields to control AI agent safety: terminalAgentExecutionEnabled (disables executable agent runs while keeping AI planning/ask available) and terminalAgentConfirmMutatingCommandSets (requires confirmation for mutating operations). No i18n keys 'settings.ai.terminalAgentExecutionEnabled' or 'settings.ai.terminalAgentConfirmMutatingCommandSets' found in Rust.

### [MISSING] [low] GlobalSettingsManager: Synchronized load/save and file-change detection
- Java: src/main/java/de/kortty/core/GlobalSettingsManager.java: synchronized methods load()/save(), loadedSettingsLastModifiedMillis tracking, reloadIfChanged() method, JAXB context updated to include SnippetEditorProfile, TerminalRecordingFormat, TerminalRecordingScope
- Rust-Stand: src-tauri/src/persistence/xml_repository.rs: basic load_xml/save_xml without file-change detection or synchronization primitives.
- Lücke: Java v2.2.0 adds synchronized load/save, file modification time tracking (loadedSettingsLastModifiedMillis), and reloadIfChanged() for hot-reloading settings if external changes are detected. JAXB marshaller includes new model classes. Rust persistence uses basic JSON/XML serialization without modification-time tracking or hot-reload capability.
- Verifikation: Rust settings persistence (src-tauri/src/commands/settings_commands.rs) uses basic JSON serialization via load_json/save_json without modification-time tracking or hot-reload capability. Java GlobalSettingsManager implements synchronized load()/save() methods, loadedSettingsLastModifiedMillis tracking, reloadIfChanged() for hot-reloading if external changes detected, and JAXB context updated to include SnippetEditorProfile, TerminalRecordingFormat, TerminalRecordingScope. Rust has no equivalent mechanism.

### [MISSING] [medium] SshTtyConnector: Input activity listener support
- Java: src/main/java/de/kortty/core/SshTtyConnector.java: InputActivityListener interface (onInputActivity(byteCount)), inputActivityListeners CopyOnWriteArrayList, addInputActivityListener/removeInputActivityListener methods, notifyInputActivity() called on write()
- Rust-Stand: src-tauri/src/ssh/session.rs: no InputActivityListener equivalent or byte-write notification mechanism.
- Lücke: Java v2.2.0 adds input activity tracking through listener pattern. When bytes are written to SSH channel, listeners are notified of byte count for activity tracking (used for auto-pause in terminal recording). Rust SSH session does not expose input activity notifications.
- Verifikation: Rust ssh/session.rs does not expose InputActivityListener interface or input activity notifications. Java SshTtyConnector has InputActivityListener interface (onInputActivity(byteCount)), CopyOnWriteArrayList<InputActivityListener>, addInputActivityListener/removeInputActivityListener methods, and notifyInputActivity() called on write(). This is used for auto-pause in terminal recording. Rust has no equivalent activity tracking mechanism.

### [MISSING] [medium] SshTtyConnector: Remote directory hint resolution and home directory updates
- Java: src/main/java/de/kortty/core/SshTtyConnector.java: updateCurrentRemoteDirectoryHint(String), updateHomeRemoteDirectoryHint(String), resolveRemoteDirectoryHint(String, String), isTildeRemoteDirectoryHint(String); removed initializeCurrentRemoteDirectory() SFTP initialization (v2.2.0 change: login output now stays visible instead of querying SFTP first)
- Rust-Stand: src-tauri/src/ssh/session.rs: no updateCurrentRemoteDirectoryHint, updateHomeRemoteDirectoryHint, resolveRemoteDirectoryHint, or isTildeRemoteDirectoryHint methods.
- Lücke: Java v2.2.0 refactors directory tracking: removes SFTP-based initialization (so login output stays visible), adds updateHomeRemoteDirectoryHint() for shell-extracted home directory, and resolveRemoteDirectoryHint() static helper to normalize ~/ paths. These methods improve directory tracking for tilde expansion and relative paths without blocking on SFTP startup. Rust does not have these directory hint and resolution methods.
- Verifikation: Rust ssh/session.rs has TerminalRuntimeState tracking current_remote_directory and home_remote_directory, but lacks Java's updateCurrentRemoteDirectoryHint(), updateHomeRemoteDirectoryHint(), resolveRemoteDirectoryHint() static helper, and isTildeRemoteDirectoryHint() methods. Java v2.2.0 also removed SFTP-based directory initialization so login output stays visible. Rust implementation does not normalize ~/ paths or provide directory hint resolution helpers.

### [MISSING] [medium] SshTtyConnector: Per-connection terminal emulation (TERM variable)
- Java: src/main/java/de/kortty/core/SshTtyConnector.java line 279: channel.setPtyType(TerminalEmulationSupport.termName(connection)) instead of hardcoded 'xterm-256color'
- Rust-Stand: src-tauri/src/model/connection.rs ConnectionSettings: no terminal_emulation field.
- Lücke: Java v2.2.0 allows per-connection terminal emulation selection. The TERM environment variable is determined from connection.getSettings() instead of using hardcoded 'xterm-256color'. Requires TerminalEmulationSupport.termName() helper. Rust SSH connector does not read terminal emulation from connection settings or call a term-name resolution function.
- Verifikation: Rust ssh/session.rs line 395 hardcodes 'xterm-256color' instead of reading from connection settings. Java SshTtyConnector line 279 calls channel.setPtyType(TerminalEmulationSupport.termName(connection)) to use per-connection terminal emulation. Rust ConnectionSettings model (src-tauri/src/model/connection.rs) does not have a terminal_emulation or term_name field. Rust provides no TerminalEmulationSupport.termName() helper function.

### [PARTIAL] [high] JobScheduler AI job execution: Auto-approval refinement
- Java: src/main/java/de/kortty/jobscheduler/JobSchedulerAiSupport.java: removed blanket ai_auto_approve check, added requiresAutoApprovalForServerChange() to check command risk level before blocking execution
- Rust-Stand: src-tauri/src/jobscheduler.rs line 579: runs ai_agent_job but does not check command risk; ai_auto_approve is checked as boolean only.
- Lücke: Java v2.2.0 refines AI agent job safety: no longer blocks all commands without auto-approval, instead checks if the command is risky (risk='requires_confirmation' or 'root' or matches dangerous command patterns via TerminalAgentService.requiresConfirmationByCommandShape). Only risky commands require auto-approval. Rust implementation has ai_auto_approve field but does not implement the risk-level detection logic (requiresAutoApprovalForServerChange).
- Verifikation: Rust jobscheduler.rs has ai_auto_approve_commands field, and terminal_agent.rs has auto_approve_root_commands flag for root command detection. However, it lacks Java's refined risk-level detection logic. Java JobSchedulerAiSupport.requiresAutoApprovalForServerChange() checks command risk level (risk='requires_confirmation', 'root', or dangerous command patterns via TerminalAgentService.requiresConfirmationByCommandShape) before blocking execution. Rust does not implement this fine-grained risk-level detection - it only checks for root commands, not command shape patterns or explicit risk metadata.

### [MISSING] [medium] i18n: Logging settings labels
- Java: src/main/resources/i18n/messages_en.properties: 'settings.tab.logging=Logging'
- Rust-Stand: src/i18n/en.json: no 'settings.tab.logging' key; SettingsDialog.tsx line 22 TabId excludes 'logging'.
- Lücke: Java v2.2.0 adds i18n key for new Logging settings tab. Rust SettingsDialog has no logging tab, so no i18n keys required yet.
- Verifikation: Rust i18n en.json (src/i18n/en.json) does not contain 'settings.tab.logging' key. Java has 'settings.tab.logging=Logging'. Rust SettingsDialog tabs (src/components/dialogs/SettingsDialog.tsx) are: language, translation, ai, backup, window, terminal - no logging tab.

### [MISSING] [medium] i18n: Terminal recording UI labels
- Java: src/main/resources/i18n/messages_en.properties: 'terminal.recording.*' keys for start, stop, idle, active, autoPaused, stopped, scope dialog, error dialogs
- Rust-Stand: src/i18n/en.json: no 'terminal.recording.*' keys; src-tauri/src/commands/terminal_recording_commands.rs exists but no locale support.
- Lücke: Java v2.2.0 adds ~20 i18n keys for terminal recording UI (Video Manager, recording state labels, error messages). Rust has basic terminal recording implementation but no i18n keys for recording UI dialogs.
- Verifikation: Rust i18n en.json lacks terminal.recording.* keys for start, stop, idle, active, autoPaused, stopped, scope dialog, and error dialogs (~20 keys total). Java messages_en.properties contains all these keys: terminal.recording.start, .stop, .idle, .active, .autoPaused, .stopped, .scope.title, .scope.header, .scope.content, .error.title, .error.header, .error.notConnected, .error.noTerminal, .error.disabled. Rust has basic terminal recording implementation but no i18n for recording UI dialogs.

### [MISSING] [medium] i18n: Terminal emulation option label
- Java: src/main/resources/i18n/messages_en.properties: 'quickConnect.terminalEmulation=Terminal emulation:' and 'connEdit.terminalEmulation=Terminal emulation:'
- Rust-Stand: src/components/dialogs/ConnectionEditor.tsx: no terminal emulation selector; src/i18n/en.json: no quickConnect.terminalEmulation or connEdit.terminalEmulation keys.
- Lücke: Java v2.2.0 adds labels for per-connection terminal emulation selector in Quick Connect and Connection Editor dialogs. Rust ConnectionEditor does not have a terminal emulation field or selector.
- Verifikation: Rust i18n en.json does not contain 'quickConnect.terminalEmulation' or 'connEdit.terminalEmulation' keys. Java has both for per-connection terminal emulation selector in Quick Connect and Connection Editor dialogs. Rust ConnectionEditor component does not have a terminal emulation field or selector, so no i18n keys are needed yet.

### [MISSING] [medium] i18n: Appearance settings tab and app design choices
- Java: src/main/resources/i18n/messages_en.properties: 'settings.tab.appearance', 'settings.appearance.*' keys for app design selector with 5 design names and preview label
- Rust-Stand: src/components/dialogs/SettingsDialog.tsx: TabId does not include 'appearance'; src/i18n/en.json: no settings.tab.appearance or settings.appearance.* keys.
- Lücke: Java v2.2.0 adds appearance tab with app design selector (Normal, Matrix Terminal, Holographic Interface, Klingon Tactical, Elegant Dark) and preview. Rust has no appearance tab or i18n keys for designs.
- Verifikation: Rust i18n en.json lacks 'settings.tab.appearance' and 'settings.appearance.*' keys for app design selector with 5 design names (Normal, Matrix Terminal, Holographic Interface, Klingon Tactical, Elegant Dark) and preview label. Java messages_en.properties contains all these keys. Rust SettingsDialog has no appearance tab or AppDesign enum in frontend.

### [MISSING] [medium] i18n: AI agent safety and execution control
- Java: src/main/resources/i18n/messages_en.properties: 'settings.ai.terminalAgentExecutionEnabled', 'settings.ai.terminalAgentConfirmMutatingCommandSets' with hints
- Rust-Stand: src-tauri/src/model/settings.rs: no terminal_agent_execution_enabled or terminal_agent_confirm_mutating_command_sets; src/i18n/en.json: no settings.ai.terminalAgentExecutionEnabled or terminalAgentConfirmMutatingCommandSets keys.
- Lücke: Java v2.2.0 adds i18n keys for two new AI safety settings (disabling executable agent runs, confirming mutating commands). Rust settings do not have these safety-gate fields.
- Verifikation: Rust i18n en.json does not contain 'settings.ai.terminalAgentExecutionEnabled' or 'settings.ai.terminalAgentConfirmMutatingCommandSets' keys with hints. Java messages_en.properties includes both with detailed hints. Rust GlobalSettings lacks these safety-gate fields, so no UI or i18n keys are currently present.

### [MISSING] [high] Logback configuration: Dynamic log directory and rotation
- Java: src/main/resources/logback.xml: KORTTY_LOG_DIR property (default ${user.home}/.kortty/logs), TimeBasedRollingPolicy with fileNamePattern kortty.%d{yyyy-MM-dd}.log (removed maxHistory=7 hardcoded value; managed by LoggingConfiguration instead)
- Rust-Stand: src-tauri/src: no Logback equivalent; logging.rs does not integrate with settings-based configuration.
- Lücke: Java v2.2.0 updates logback.xml to use KORTTY_LOG_DIR system property (set by LoggingConfiguration.bootstrapFromPersistedSettings), removes hardcoded maxHistory value, and delegates retention/compression to LoggingConfiguration.maintainLogDirectory(). Rust has no equivalent Logback or centralized logging configuration system.
- Verifikation: Rust does not use Logback or have equivalent logback.xml configuration. Rust TerminalLogger (src-tauri/src/logging.rs) handles session log compression but not centralized application logging configuration. Java logback.xml uses KORTTY_LOG_DIR system property (set by LoggingConfiguration.bootstrapFromPersistedSettings), removed hardcoded maxHistory value, delegates retention/compression to LoggingConfiguration.maintainLogDirectory(). Rust lacks centralized logging configuration system, Logback integration, and KORTTY_LOG_DIR property management.

## Bereich: coverage-critic (10 Lücken)

### [MISSING] [high] AI Profile Local CLI Connection Mode
- Java: src/main/java/de/kortty/model/AiProfile.java (AiConnectionMode enum, connectionMode field, cliProviderId, cliExecutablePath, cliArgumentsTemplate), src/main/java/de/kortty/core/AiCliProviderDescriptor.java, src/main/java/de/kortty/core/LocalCliAiService.java
- Lücke: AiProfile model lacks connectionMode enum, CLI provider fields (cliProviderId, cliExecutablePath, cliArgumentsTemplate), and LocalCliAiService implementation for executing AI requests through local CLI providers rather than HTTP API endpoints.

### [MISSING] [medium] AI Reasoning Effort Discovery
- Java: src/main/java/de/kortty/model/AiProfile.java (discoveredReasoningEfforts, reasoningDiscoveryKey fields), src/main/java/de/kortty/core/AiReasoningDiscoveryService.java
- Lücke: AiProfile model lacks discoveredReasoningEfforts list and reasoningDiscoveryKey to support runtime discovery of which reasoning efforts the AI provider supports. AiReasoningDiscoveryService is not implemented.

### [MISSING] [medium] Terminal Colors Enable/Disable
- Java: src/main/java/de/kortty/model/ConnectionSettings.java (terminalColorsEnabled field)
- Lücke: ConnectionSettings model lacks terminalColorsEnabled boolean field to allow disabling ANSI and TrueColor sequences while keeping text/background colors.

### [MISSING] [medium] Job Scheduler Risk-based AI Command Approval
- Java: src/main/java/de/kortty/jobscheduler/JobSchedulerAiSupport.java (requiresAutoApprovalForServerChange, normalizeRisk methods)
- Lücke: Job scheduler AI agent logic lacks risk assessment for commands to determine if auto-approval is required based on command shape and risk level. The approval check was refactored from blanket to risk-based in v2.1.0..main.

### [MISSING] [low] Job Scheduler AI Agent Auto-Approval Default Migration
- Java: src/main/java/de/kortty/jobscheduler/JobSchedulerRepository.java (aiAgentAutoApproveDefaultMigrated field, migrateAiAgentAutoApproveDefault method)
- Lücke: JobSchedulerStateFile lacks migration flag and logic to default aiAutoApproveCommands to true for existing AI agent jobs during loading.

### [PARTIAL] [low] Terminal-only Fullscreen Mode
- Java: src/main/java/de/kortty/ui/MainWindow.java, src/main/resources/styles/terminal.css (terminal-only-fullscreen style rules), src/main/resources/i18n/messages.properties (menu.view.terminalOnlyFullscreen, menu.view.hideTerminalScrollbarsFullscreen)
- Lücke: Rust frontend has terminalOnlyFullscreen state in MainWindow but lacks CSS styling rules for hiding tab headers in fullscreen and related i18n strings for terminal-only fullscreen menu items.

### [MISSING] [low] Formatter Manifest Version Metadata
- Java: src/main/resources/formatters/formatter-manifest.properties (new file with version/source metadata for google-java-format, node, shfmt, prettier, sql-formatter, perltidy)
- Lücke: Rust port lacks formatter manifest file or equivalent metadata tracking for external formatter versions and download sources.

### [MISSING] [low] File Browser CSS Styling
- Java: src/main/resources/styles/filebrowser.css (new file with tree styling rules)
- Lücke: New filebrowser.css not ported to Rust. LocalFileBrowser component in Rust exists but lacks dedicated CSS styling definitions for file tree appearance.

### [PARTIAL] [medium] Internationalization Extensions
- Java: src/main/resources/i18n/messages*.properties (new keys for appearance settings tab, filebrowser labels, terminal emulation field, logging/updates tabs, etc.)
- Lücke: Rust i18n files (src/i18n/*.json) lack many new translation keys added in main: settings.appearance.*, filebrowser.*, menu.view.terminalOnlyFullscreen, quickConnect.terminalEmulation, settings.tab.logging, settings.tab.updates, settings.tab.appearance, terminal.dragDrop.target, and many new filebrowser context menu and status strings.

### [MISSING] [low] Quick Connect Terminal Emulation Field
- Java: src/main/java/de/kortty/ui/QuickConnectDialog.java, src/main/resources/i18n/messages.properties (quickConnect.terminalEmulation)
- Lücke: Quick Connect dialog lacks terminal emulation type selection field that was added to the Java UI.