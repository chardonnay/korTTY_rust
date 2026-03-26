import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { Bot, Copy, Download, Loader2, RotateCcw, Save, Send, Wand2, X, ZoomIn, ZoomOut } from "lucide-react";
import type {
  AiExecutionResult,
  AiProfile,
  AiRequestPayload,
  AiTokenUsageSnapshot,
  SavedAiChat,
  SavedAiChatMessage,
} from "../../types/ai";
import type { GlobalSettings } from "../../store/settingsStore";
import {
  AI_LANGUAGE_OPTIONS,
  getAiLanguageLabel,
  normalizeLanguageCode,
  resolveGuiLanguageCode,
} from "../../utils/aiLanguage";
import { AiCodeBlockDialog } from "./AiCodeBlockDialog";
import { AiCodeBlockPreview } from "./AiCodeBlockPreview";

interface AiChatTabProps {
  tabId: string;
  initialRequest?: AiRequestPayload;
  initialChat?: SavedAiChat;
  onTitleChange: (title: string) => void;
  onSavedChatChange: (savedChat: SavedAiChat) => void;
}

interface ParsedCodeBlock {
  id: string;
  language: string;
  code: string;
}

interface ParsedMarkdownTable {
  id: string;
  headers: string[];
  rows: string[][];
}

interface ChatContextMenuState {
  x: number;
  y: number;
  selectionText: string;
  editableSelection: string;
  canPaste: boolean;
  canCut: boolean;
}

interface PendingAiRequest {
  request: AiRequestPayload;
  appendUserPrompt: boolean;
}

const DEFAULT_CHAT_FONT_SIZE_PX = 14;
const MIN_CHAT_FONT_SIZE_PX = 11;
const MAX_CHAT_FONT_SIZE_PX = 22;

type MessageSegment =
  | { type: "text"; id: string; text: string }
  | { type: "code"; id: string; block: ParsedCodeBlock }
  | { type: "table"; id: string; table: ParsedMarkdownTable };

function createMessage(
  role: SavedAiChatMessage["role"],
  content: string,
  aiProfileId?: string,
  aiProfileName?: string,
): SavedAiChatMessage {
  return {
    role,
    content,
    createdAt: Date.now(),
    aiProfileId,
    aiProfileName,
  };
}

function buildConversationContext(messages: SavedAiChatMessage[]) {
  return messages
    .map((message) => `${message.role === "User" ? "User" : "Assistant"}: ${message.content}`)
    .join("\n\n");
}

function buildPlainTranscript(messages: SavedAiChatMessage[]) {
  return messages
    .map((message) => `[${message.role === "User" ? "User" : message.aiProfileName || "Assistant"}]\n${message.content}`)
    .join("\n\n");
}

function buildMarkdownTranscript(title: string, messages: SavedAiChatMessage[]) {
  const sections = messages.map((message) => {
    const heading = message.role === "User" ? "User" : message.aiProfileName || "Assistant";
    return `## ${heading}\n\n${message.content}`;
  });
  return `# ${title}\n\n${sections.join("\n\n")}\n`;
}

function parseMarkdownTableRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function isMarkdownTableRow(line: string): boolean {
  return line.includes("|") && parseMarkdownTableRow(line).length >= 2;
}

function isMarkdownTableSeparator(line: string): boolean {
  const cells = parseMarkdownTableRow(line);
  return cells.length >= 2 && cells.every((cell) => /^:?-{3,}:?$/.test(cell));
}

function normalizeTableRows(headers: string[], rows: string[][]): string[][] {
  return rows.map((row) => {
    const normalized = [...row];
    while (normalized.length < headers.length) {
      normalized.push("");
    }
    return normalized.slice(0, headers.length);
  });
}

function splitTextAndTableSegments(text: string, messageKey: string): MessageSegment[] {
  const lines = text.split("\n");
  const segments: MessageSegment[] = [];
  const bufferedLines: string[] = [];
  let segmentIndex = 0;

  function flushBufferedText() {
    if (bufferedLines.length === 0) {
      return;
    }
    const bufferedText = bufferedLines.join("\n");
    bufferedLines.length = 0;
    if (!bufferedText.trim()) {
      return;
    }
    segments.push({
      type: "text",
      id: `${messageKey}-text-${segmentIndex++}`,
      text: bufferedText,
    });
  }

  for (let index = 0; index < lines.length; ) {
    if (
      index + 1 < lines.length &&
      isMarkdownTableRow(lines[index]) &&
      isMarkdownTableSeparator(lines[index + 1])
    ) {
      const headers = parseMarkdownTableRow(lines[index]);
      const rows: string[][] = [];
      let nextIndex = index + 2;
      while (nextIndex < lines.length && isMarkdownTableRow(lines[nextIndex])) {
        rows.push(parseMarkdownTableRow(lines[nextIndex]));
        nextIndex += 1;
      }

      if (headers.length >= 2 && rows.length > 0) {
        flushBufferedText();
        const tableSegmentId = `${messageKey}-table-${segmentIndex++}`;
        segments.push({
          type: "table",
          id: tableSegmentId,
          table: {
            id: tableSegmentId,
            headers,
            rows: normalizeTableRows(headers, rows),
          },
        });
        index = nextIndex;
        continue;
      }
    }

    bufferedLines.push(lines[index]);
    index += 1;
  }

  flushBufferedText();
  return segments;
}

function splitMessageSegments(content: string, messageKey: string): MessageSegment[] {
  const pattern = /```([^\n`]*)\n([\s\S]*?)```/g;
  const segments: MessageSegment[] = [];
  let lastIndex = 0;
  let blockIndex = 0;

  for (const match of content.matchAll(pattern)) {
    const rawStart = match.index ?? 0;
    const textBefore = content.slice(lastIndex, rawStart);
    segments.push(
      ...splitTextAndTableSegments(textBefore, `${messageKey}-part-${blockIndex}`),
    );

    segments.push({
      type: "code",
      id: `${messageKey}-code-${blockIndex}`,
      block: {
        id: `${messageKey}-code-${blockIndex}`,
        language: match[1]?.trim().toLowerCase() || "plain",
        code: match[2]?.replace(/\n$/, "") || "",
      },
    });

    lastIndex = rawStart + match[0].length;
    blockIndex += 1;
  }

  segments.push(
    ...splitTextAndTableSegments(
      content.slice(lastIndex),
      `${messageKey}-part-${blockIndex}`,
    ),
  );
  return segments;
}

function tableToClipboardText(table: ParsedMarkdownTable): string {
  return [table.headers, ...table.rows]
    .map((row) => row.map((cell) => cell.replace(/\s+/g, " ").trim()).join("\t"))
    .join("\n");
}

function getTextSelectionText(): string {
  return window.getSelection()?.toString() || "";
}

function resolveContextCopyText(target: EventTarget | null): string {
  if (!(target instanceof HTMLElement)) {
    return "";
  }

  const copySource = target.closest<HTMLElement>("[data-copy-text]");
  return copySource?.dataset.copyText || "";
}

function isInsideCodeDialog(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && target.closest("[data-ai-code-dialog='true']") != null;
}

function selectionBelongsToChat(root: HTMLElement | null, selection: Selection | null): boolean {
  if (!root || !selection || selection.rangeCount === 0) {
    return false;
  }

  const anchorNode = selection.anchorNode;
  const focusNode = selection.focusNode;
  return (
    (anchorNode != null && root.contains(anchorNode)) ||
    (focusNode != null && root.contains(focusNode))
  );
}

function clampChatFontSize(fontSizePx: number): number {
  return Math.min(MAX_CHAT_FONT_SIZE_PX, Math.max(MIN_CHAT_FONT_SIZE_PX, fontSizePx));
}

function formatWaitingSince(timestampMs: number): string {
  return new Date(timestampMs).toLocaleTimeString();
}

function formatElapsedDuration(elapsedMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) {
    return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

function formatChatTimestamp(timestampMs: number): string {
  return new Date(timestampMs).toLocaleString();
}

function fallbackChatTitle(messages: SavedAiChatMessage[]): string {
  const userMessage = messages.find((message) => message.role === "User")?.content.trim();
  if (!userMessage) {
    return "AI Chat";
  }
  return userMessage.length > 80 ? `${userMessage.slice(0, 77).trimEnd()}...` : userMessage;
}

export function AiChatTab({
  tabId,
  initialRequest,
  initialChat,
  onTitleChange,
  onSavedChatChange,
}: AiChatTabProps) {
  const initialResponseLanguageCode =
    initialChat?.responseLanguageCode || initialRequest?.responseLanguageCode || "";
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState(
    initialChat?.activeAiProfileId || initialRequest?.profileId || "",
  );
  const [messages, setMessages] = useState<SavedAiChatMessage[]>(initialChat?.messages || []);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string>("Ready");
  const [savedChatId, setSavedChatId] = useState(initialChat?.id || "");
  const [chatCreatedAt] = useState(initialChat?.createdAt || Date.now());
  const [title, setTitle] = useState(
    initialChat?.title ||
      (initialRequest?.action === "Summarize"
        ? "AI Summary"
        : initialRequest?.action === "SolveProblem"
          ? "AI Problem Analysis"
          : "AI Chat"),
  );
  const [selectedText] = useState(initialChat?.selectedText || initialRequest?.selectedText || "");
  const [connectionDisplayName] = useState(initialChat?.connectionDisplayName || initialRequest?.connectionDisplayName);
  const [responseLanguageCode, setResponseLanguageCode] = useState(
    normalizeLanguageCode(initialResponseLanguageCode),
  );
  const [usageSnapshot, setUsageSnapshot] = useState<AiTokenUsageSnapshot | undefined>();
  const [selectedCodeBlock, setSelectedCodeBlock] = useState<ParsedCodeBlock | null>(null);
  const [contextMenu, setContextMenu] = useState<ChatContextMenuState | null>(null);
  const [currentRequestId, setCurrentRequestId] = useState<string | null>(null);
  const [chatFontSizePx, setChatFontSizePx] = useState(DEFAULT_CHAT_FONT_SIZE_PX);
  const [chatSelectionText, setChatSelectionText] = useState("");
  const [waitingStartedAt, setWaitingStartedAt] = useState<number | null>(null);
  const [waitingNow, setWaitingNow] = useState(() => Date.now());
  const initialRunRef = useRef(false);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);
  const lastRequestRef = useRef<PendingAiRequest | null>(null);
  const chatRootRef = useRef<HTMLDivElement | null>(null);

  const closeContextMenu = useCallback(() => {
    setContextMenu(null);
  }, []);

  const loadProfiles = useCallback(async () => {
    try {
      const loaded = await invoke<AiProfile[]>("get_ai_profiles");
      setProfiles(loaded);
      setSelectedProfileId((current) => current || loaded[0]?.id || "");
    } catch (error) {
      setStatus(`Failed to load AI profiles: ${String(error)}`);
    }
  }, []);

  const runRequest = useCallback(async (request: AiRequestPayload, appendUserPrompt: boolean) => {
    const requestId = crypto.randomUUID();
    const requestSnapshot: PendingAiRequest = {
      request,
      appendUserPrompt,
    };
    lastRequestRef.current = requestSnapshot;
    setCurrentRequestId(requestId);
    setBusy(true);
    setStatus("Waiting for AI response...");
    setWaitingStartedAt(Date.now());
    const trimmedUserPrompt = request.userPrompt?.trim();
    if (appendUserPrompt && trimmedUserPrompt) {
      setMessages((current) => [
        ...current,
        createMessage("User", trimmedUserPrompt),
      ]);
    }

    try {
      const result = await invoke<AiExecutionResult>("execute_ai_action", {
        request,
        requestId,
      });
      setMessages((current) => [
        ...current,
        createMessage(
          "Assistant",
          result.content,
          result.activeProfileId,
          result.activeProfileName,
        ),
      ]);
      setUsageSnapshot(result.usageSnapshot);
      if (result.activeProfileId) {
        setSelectedProfileId(result.activeProfileId);
      }
      setStatus(
        result.usageSnapshot
          ? `Completed. Used ${result.usageSnapshot.usedTotalTokens.toLocaleString()} total tokens in the current cycle.`
          : "Completed.",
      );
      void loadProfiles();
    } catch (error) {
      const errorMessage = String(error);
      if (errorMessage.toLowerCase().includes("cancel")) {
        setStatus("AI request cancelled.");
        return;
      }
      setMessages((current) => [
        ...current,
        createMessage("Assistant", `AI request failed: ${errorMessage}`),
      ]);
      setStatus("AI request failed.");
    } finally {
      setCurrentRequestId((current) => (current === requestId ? null : current));
      setBusy(false);
      setWaitingStartedAt(null);
    }
  }, [loadProfiles]);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

  useEffect(() => {
    if (initialResponseLanguageCode.trim()) {
      return;
    }
    let cancelled = false;

    async function loadDefaultLanguage() {
      try {
        const settings = await invoke<GlobalSettings>("get_settings");
        if (!cancelled) {
          setResponseLanguageCode(resolveGuiLanguageCode(settings));
        }
      } catch {
        if (!cancelled) {
          setResponseLanguageCode(resolveGuiLanguageCode(null));
        }
      }
    }

    void loadDefaultLanguage();
    return () => {
      cancelled = true;
    };
  }, [initialResponseLanguageCode]);

  useEffect(() => {
    onTitleChange(title);
  }, [onTitleChange, title]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [messages, busy]);

  useEffect(() => {
    if (!initialRequest || initialChat || initialRunRef.current) return;
    if (!selectedProfileId) return;
    initialRunRef.current = true;
    void runRequest(initialRequest, true);
  }, [connectionDisplayName, initialChat, initialRequest, responseLanguageCode, runRequest, selectedProfileId, selectedText]);

  const activeProfile = useMemo(
    () => profiles.find((profile) => profile.id === selectedProfileId) ?? null,
    [profiles, selectedProfileId],
  );
  const usageProgress = useMemo(() => {
    if (!usageSnapshot || usageSnapshot.unlimited || usageSnapshot.maxTokens <= 0) {
      return 0;
    }
    return Math.min(100, (usageSnapshot.usedTotalTokens / usageSnapshot.maxTokens) * 100);
  }, [usageSnapshot]);

  useEffect(() => {
    if (!contextMenu) {
      return;
    }

    function handleWindowMouseDown(event: MouseEvent) {
      if (contextMenuRef.current && !contextMenuRef.current.contains(event.target as Node)) {
        closeContextMenu();
      }
    }

    function handleWindowKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key === "Escape") {
        closeContextMenu();
      }
    }

    window.addEventListener("mousedown", handleWindowMouseDown);
    window.addEventListener("keydown", handleWindowKeyDown);
    return () => {
      window.removeEventListener("mousedown", handleWindowMouseDown);
      window.removeEventListener("keydown", handleWindowKeyDown);
    };
  }, [closeContextMenu, contextMenu]);

  useEffect(() => {
    function handleSelectionChange() {
      const selection = window.getSelection();
      if (selectionBelongsToChat(chatRootRef.current, selection)) {
        setChatSelectionText(selection?.toString() || "");
        return;
      }
      setChatSelectionText("");
    }

    document.addEventListener("selectionchange", handleSelectionChange);
    return () => {
      document.removeEventListener("selectionchange", handleSelectionChange);
    };
  }, []);

  useEffect(() => {
    if (!busy || waitingStartedAt == null) {
      return;
    }

    setWaitingNow(Date.now());
    const intervalId = window.setInterval(() => {
      setWaitingNow(Date.now());
    }, 1000);

    return () => {
      window.clearInterval(intervalId);
    };
  }, [busy, waitingStartedAt]);

  useEffect(() => {
    function handleWindowKeyDown(event: globalThis.KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) {
        return;
      }

      if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        setChatFontSizePx((current) => clampChatFontSize(current + 1));
        return;
      }

      if (event.key === "-" || event.key === "_") {
        event.preventDefault();
        setChatFontSizePx((current) => clampChatFontSize(current - 1));
        return;
      }

      if (event.key === "0") {
        event.preventDefault();
        setChatFontSizePx(DEFAULT_CHAT_FONT_SIZE_PX);
      }
    }

    window.addEventListener("keydown", handleWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", handleWindowKeyDown);
    };
  }, []);

  function getEditableSelectionText(): string {
    const textarea = inputRef.current;
    if (!textarea) {
      return "";
    }
    const start = textarea.selectionStart ?? 0;
    const end = textarea.selectionEnd ?? 0;
    if (start === end) {
      return "";
    }
    return input.slice(Math.min(start, end), Math.max(start, end));
  }

  function canPasteFromClipboard(): boolean {
    const textarea = inputRef.current;
    return textarea != null && document.activeElement === textarea;
  }

  function openChatContextMenu(event: ReactMouseEvent<HTMLElement>) {
    if (isInsideCodeDialog(event.target)) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();

    const selectionText =
      getTextSelectionText() ||
      chatSelectionText ||
      resolveContextCopyText(event.target);
    const editableSelection = getEditableSelectionText();
    setContextMenu({
      x: event.clientX,
      y: event.clientY,
      selectionText,
      editableSelection,
      canPaste: canPasteFromClipboard(),
      canCut: editableSelection.length > 0,
    });
  }

  async function handleCopySelection() {
    if (!contextMenu) {
      return;
    }
    const text = contextMenu.editableSelection || contextMenu.selectionText;
    if (!text) {
      return;
    }

    try {
      await navigator.clipboard.writeText(text);
      closeContextMenu();
      setStatus("Selection copied to clipboard.");
    } catch (error) {
      setStatus(`Copy failed: ${String(error)}`);
    }
  }

  async function handlePasteSelection() {
    if (!contextMenu || !contextMenu.canPaste) {
      return;
    }
    try {
      const text = await navigator.clipboard.readText();
      if (!text) {
        return;
      }

      const textarea = inputRef.current;
      if (!textarea) {
        return;
      }

      const start = textarea.selectionStart ?? input.length;
      const end = textarea.selectionEnd ?? input.length;
      const nextValue = `${input.slice(0, Math.min(start, end))}${text}${input.slice(Math.max(start, end))}`;
      setInput(nextValue);
      closeContextMenu();
      setStatus("Clipboard pasted into chat input.");

      window.requestAnimationFrame(() => {
        textarea.focus();
        const caret = Math.min(start, end) + text.length;
        textarea.setSelectionRange(caret, caret);
      });
    } catch (error) {
      setStatus(`Paste failed: ${String(error)}`);
    }
  }

  async function handleCutSelection() {
    if (!contextMenu || !contextMenu.canCut) {
      return;
    }
    const text = getEditableSelectionText();
    if (!text) {
      return;
    }

    const textarea = inputRef.current;
    if (!textarea) {
      return;
    }

    const start = Math.min(textarea.selectionStart ?? 0, textarea.selectionEnd ?? 0);
    const end = Math.max(textarea.selectionStart ?? 0, textarea.selectionEnd ?? 0);
    try {
      await navigator.clipboard.writeText(text);
      setInput(`${input.slice(0, start)}${input.slice(end)}`);
      closeContextMenu();
      setStatus("Selection cut to clipboard.");

      window.requestAnimationFrame(() => {
        textarea.focus();
        textarea.setSelectionRange(start, start);
      });
    } catch (error) {
      setStatus(`Cut failed: ${String(error)}`);
    }
  }

  async function handleGenerateTitle() {
    if (!selectedProfileId || busy || messages.length === 0) return;
    setBusy(true);
    setStatus("Generating chat title...");
    try {
      const result = await invoke<AiExecutionResult>("execute_ai_action", {
        request: {
          action: "GenerateChatTitle",
          profileId: selectedProfileId,
          selectedText,
          connectionDisplayName,
          responseLanguageCode,
          conversationContext: buildConversationContext(messages),
        } satisfies AiRequestPayload,
        requestId: crypto.randomUUID(),
      });
      const generatedTitle = result.content.trim().replace(/^["']|["']$/g, "");
      if (generatedTitle) {
        setTitle(generatedTitle);
        setStatus(`Generated title "${generatedTitle}".`);
      } else {
        setStatus("The AI returned an empty title.");
      }
      setUsageSnapshot(result.usageSnapshot);
      void loadProfiles();
    } catch (error) {
      setStatus(`Title generation failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function generateChatTitleForSave(): Promise<string> {
    if (!selectedProfileId || messages.length === 0) {
      return title || fallbackChatTitle(messages);
    }

    const result = await invoke<AiExecutionResult>("execute_ai_action", {
      request: {
        action: "GenerateChatTitle",
        profileId: selectedProfileId,
        selectedText,
        connectionDisplayName,
        responseLanguageCode,
        conversationContext: buildConversationContext(messages),
      } satisfies AiRequestPayload,
      requestId: crypto.randomUUID(),
    });

    setUsageSnapshot(result.usageSnapshot);
    if (result.activeProfileId) {
      setSelectedProfileId(result.activeProfileId);
    }
    void loadProfiles();

    const generatedTitle = result.content.trim().replace(/^["']|["']$/g, "");
    return generatedTitle || title || fallbackChatTitle(messages);
  }

  async function handleSendFollowUp() {
    if (!selectedProfileId || !input.trim() || busy) return;
    const prompt = input.trim();
    setInput("");
    await runRequest(
      {
        action: "Ask",
        profileId: selectedProfileId,
        selectedText,
        connectionDisplayName,
        responseLanguageCode,
        userPrompt: prompt,
        conversationContext: buildConversationContext(messages),
      },
      true,
    );
  }

  async function handleRetryLastRequest() {
    if (busy || !lastRequestRef.current) {
      return;
    }

    const lastRequest = lastRequestRef.current;
    const retryRequest: AiRequestPayload = {
      ...lastRequest.request,
      profileId: selectedProfileId || lastRequest.request.profileId,
    };
    await runRequest(retryRequest, lastRequest.appendUserPrompt);
  }

  async function handleCancelCurrentRequest() {
    if (!currentRequestId) {
      return;
    }

    try {
      await invoke("cancel_ai_request", { requestId: currentRequestId });
      setStatus("Cancelling AI request...");
    } catch (error) {
      setStatus(`Cancel failed: ${String(error)}`);
    }
  }

  async function handleCopyTranscript() {
    try {
      await navigator.clipboard.writeText(buildPlainTranscript(messages));
      setStatus("Transcript copied to clipboard.");
    } catch (error) {
      setStatus(`Copy failed: ${String(error)}`);
    }
  }

  async function handleCopyTable(table: ParsedMarkdownTable) {
    try {
      await navigator.clipboard.writeText(tableToClipboardText(table));
      setStatus("Table copied to clipboard.");
    } catch (error) {
      setStatus(`Copy failed: ${String(error)}`);
    }
  }

  async function handleCopyTableCell(value: string) {
    try {
      await navigator.clipboard.writeText(value);
      setStatus("Table cell copied to clipboard.");
    } catch (error) {
      setStatus(`Copy failed: ${String(error)}`);
    }
  }

  async function handleExport(kind: "text" | "markdown") {
    const path = await saveDialog({
      defaultPath: `${title || "ai-chat"}.${kind === "markdown" ? "md" : "txt"}`,
      filters: [
        { name: kind === "markdown" ? "Markdown" : "Text", extensions: [kind === "markdown" ? "md" : "txt"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (!path) return;

    try {
      const content = kind === "markdown"
        ? buildMarkdownTranscript(title, messages)
        : buildPlainTranscript(messages);
      await writeTextFile(path, content);
      setStatus(`Exported transcript to ${path}.`);
    } catch (error) {
      console.error("Failed to export AI chat transcript:", error);
      setStatus(`Export failed: ${String(error)}`);
    }
  }

  async function handleSaveChat() {
    if (busy || messages.length === 0) {
      return;
    }
    setBusy(true);
    setStatus("Generating chat title...");
    try {
      const nextTitle = await generateChatTitleForSave();
      const trimmedTitle = nextTitle.trim() || fallbackChatTitle(messages);
      const saved = await invoke<SavedAiChat>("save_ai_chat", {
        chat: {
          id: savedChatId,
          title: trimmedTitle,
          createdAt: chatCreatedAt,
          updatedAt: Date.now(),
          selectedText,
          connectionDisplayName,
          responseLanguageCode,
          activeAiProfileId: activeProfile?.id,
          activeAiProfileName: activeProfile?.name,
          messages,
        },
      });
      setSavedChatId(saved.id);
      setTitle(saved.title);
      onSavedChatChange(saved);
      setStatus(`Saved chat "${saved.title}".`);
    } catch (error) {
      setStatus(`Save failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  function handleInputKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>) {
    if (event.key !== "Enter" || busy || !selectedProfileId || !input.trim()) {
      return;
    }

    const hasMultipleLines = input.includes("\n");
    if (hasMultipleLines) {
      if (!event.metaKey && !event.ctrlKey) {
        return;
      }
      event.preventDefault();
      void handleSendFollowUp();
      return;
    }

    if (event.shiftKey || event.altKey || event.ctrlKey) {
      return;
    }

    event.preventDefault();
    void handleSendFollowUp();
  }

  function handleIncreaseFontSize() {
    setChatFontSizePx((current) => clampChatFontSize(current + 1));
  }

  function handleDecreaseFontSize() {
    setChatFontSizePx((current) => clampChatFontSize(current - 1));
  }

  function handleResetFontSize() {
    setChatFontSizePx(DEFAULT_CHAT_FONT_SIZE_PX);
  }

  const waitingMeta = useMemo(() => {
    if (!busy || waitingStartedAt == null) {
      return null;
    }

    return {
      startedAtLabel: formatWaitingSince(waitingStartedAt),
      elapsedLabel: formatElapsedDuration(waitingNow - waitingStartedAt),
    };
  }, [busy, waitingNow, waitingStartedAt]);

  return (
    <div
      ref={chatRootRef}
      className="flex flex-col h-full min-h-0 bg-kortty-terminal"
      onContextMenuCapture={openChatContextMenu}
    >
      <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-kortty-border bg-kortty-surface">
        <div className="min-w-0">
          <div className="text-sm font-semibold truncate">{title}</div>
          <div className="text-[11px] text-kortty-text-dim truncate">
            {connectionDisplayName || "No connection label"} | {selectedText.length.toLocaleString()} chars
          </div>
          {usageSnapshot && (
            <div className="mt-2 space-y-1 max-w-[360px]">
              <div className="flex items-center justify-between text-[10px] text-kortty-text-dim">
                <span>
                  {usageSnapshot.unlimited
                    ? `${usageSnapshot.usedTotalTokens.toLocaleString()} tokens used`
                    : `${usageSnapshot.usedTotalTokens.toLocaleString()} / ${usageSnapshot.maxTokens.toLocaleString()} tokens`}
                </span>
                {!usageSnapshot.unlimited && usageSnapshot.remainingTokens != null && (
                  <span>{usageSnapshot.remainingTokens.toLocaleString()} remaining</span>
                )}
              </div>
              {!usageSnapshot.unlimited && (
                <div className="h-1.5 rounded bg-kortty-panel overflow-hidden">
                  <div
                    className={`h-full ${
                      usageSnapshot.warningLevel === "Red"
                        ? "bg-kortty-error"
                        : usageSnapshot.warningLevel === "Yellow"
                          ? "bg-kortty-warning"
                          : "bg-kortty-success"
                    }`}
                    style={{ width: `${usageProgress}%` }}
                  />
                </div>
              )}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <select
            className="input-field text-xs w-52"
            value={selectedProfileId}
            onChange={(event) => setSelectedProfileId(event.target.value)}
            disabled={busy || profiles.length === 0}
          >
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.name || "Unnamed profile"}
              </option>
            ))}
          </select>
          <select
            className="input-field text-xs w-36"
            value={responseLanguageCode}
            onChange={(event) => {
              const nextLanguageCode = normalizeLanguageCode(event.target.value);
              setResponseLanguageCode(nextLanguageCode);
              setStatus(`Response language set to ${getAiLanguageLabel(nextLanguageCode)}.`);
            }}
            disabled={busy}
          >
            {AI_LANGUAGE_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2 disabled:opacity-50"
            onClick={() => void handleGenerateTitle()}
            disabled={busy || !selectedProfileId || messages.length === 0}
          >
            <Wand2 className="w-3.5 h-3.5" />
            Auto Title
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
            onClick={() => void handleCopyTranscript()}
          >
            <Copy className="w-3.5 h-3.5" />
            Copy
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
            onClick={() => void handleExport("text")}
          >
            <Download className="w-3.5 h-3.5" />
            TXT
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2"
            onClick={() => void handleExport("markdown")}
          >
            <Download className="w-3.5 h-3.5" />
            MD
          </button>
          <button
            className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2 disabled:opacity-50"
            onClick={() => void handleSaveChat()}
            disabled={messages.length === 0}
          >
            <Save className="w-3.5 h-3.5" />
            Save
          </button>
          <div className="ml-1 flex items-center gap-1 rounded border border-kortty-border bg-kortty-panel/60 px-1 py-1">
            <button
              className="rounded px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
              onClick={handleDecreaseFontSize}
              title="Reduce chat font size (Cmd/Ctrl+-)"
            >
              <ZoomOut className="h-3.5 w-3.5" />
            </button>
            <button
              className="rounded px-2 py-1 text-[11px] text-kortty-text transition-colors hover:bg-kortty-border"
              onClick={handleResetFontSize}
              title="Reset chat font size (Cmd/Ctrl+0)"
            >
              Reset
            </button>
            <button
              className="rounded px-2 py-1 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
              onClick={handleIncreaseFontSize}
              title="Increase chat font size (Cmd/Ctrl++)"
            >
              <ZoomIn className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
        {messages.length === 0 && !busy && (
          <div className="h-full flex items-center justify-center text-sm text-kortty-text-dim">
            No AI messages yet.
          </div>
        )}
        {messages.map((message, index) => {
          const messageKey = `${tabId}-${index}-${message.createdAt}`;
          const segments =
            message.role === "Assistant"
              ? splitMessageSegments(message.content, messageKey)
              : [{ type: "text", id: `${messageKey}-text`, text: message.content } satisfies MessageSegment];

          return (
            <div
              key={messageKey}
              className={`rounded-lg border px-4 py-3 ${
                message.role === "User"
                  ? "bg-kortty-surface border-kortty-border"
                  : "bg-kortty-panel/70 border-kortty-accent/30"
              }`}
            >
              <div className="mb-2 flex items-center gap-2 text-[11px] text-kortty-text-dim">
                <Bot className="h-3.5 w-3.5" />
                <span>{message.role === "User" ? "User" : message.aiProfileName || "Assistant"}</span>
                <span>{formatChatTimestamp(message.createdAt)}</span>
              </div>
              <div className="space-y-3">
                {segments.map((segment) => {
                  if (segment.type === "text") {
                    return (
                      <div
                        key={segment.id}
                        className="whitespace-pre-wrap break-words text-kortty-text font-mono leading-6"
                        style={{ fontSize: `${chatFontSizePx}px` }}
                      >
                        {segment.text}
                      </div>
                    );
                  }

                  if (segment.type === "code") {
                    return (
                      <AiCodeBlockPreview
                        key={segment.id}
                        language={segment.block.language}
                        code={segment.block.code}
                        fontSizePx={chatFontSizePx}
                        onOpenFull={() => setSelectedCodeBlock(segment.block)}
                      />
                    );
                  }

                  return (
                    <div
                      key={segment.id}
                      className="overflow-hidden rounded border border-kortty-border bg-kortty-surface/60"
                    >
                      <div className="flex items-center justify-between border-b border-kortty-border px-3 py-2">
                        <div className="text-xs font-medium text-kortty-text">Table</div>
                        <button
                          className="flex items-center gap-2 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                          onClick={() => void handleCopyTable(segment.table)}
                        >
                          <Copy className="h-3.5 w-3.5" />
                          Copy Table
                        </button>
                      </div>
                      <div className="overflow-x-auto">
                        <table
                          className="min-w-full border-collapse"
                          style={{ fontSize: `${Math.max(chatFontSizePx - 1, 12)}px` }}
                        >
                          <thead>
                            <tr className="bg-kortty-panel/70 text-left text-kortty-text">
                              {segment.table.headers.map((header, headerIndex) => (
                                <th
                                  key={`${segment.table.id}-header-${headerIndex}`}
                                  className="border-b border-kortty-border px-3 py-2 font-semibold"
                                  data-copy-text={header}
                                >
                                  {header}
                                </th>
                              ))}
                            </tr>
                          </thead>
                          <tbody>
                            {segment.table.rows.map((row, rowIndex) => (
                              <tr
                                key={`${segment.table.id}-row-${rowIndex}`}
                                className="odd:bg-kortty-terminal/20 even:bg-kortty-surface/30"
                              >
                                {row.map((cell, cellIndex) => (
                                  <td
                                    key={`${segment.table.id}-cell-${rowIndex}-${cellIndex}`}
                                    className="border-b border-kortty-border/60 p-0 align-top"
                                  >
                                    <button
                                      className="h-full w-full px-3 py-2 text-left text-kortty-text transition-colors hover:bg-kortty-accent/10"
                                      data-copy-text={cell}
                                      onClick={() => void handleCopyTableCell(cell)}
                                      title="Copy cell"
                                    >
                                      {cell || <span className="opacity-40">-</span>}
                                    </button>
                                  </td>
                                ))}
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          );
        })}
        {busy && (
          <div className="rounded-lg border border-kortty-accent/30 bg-kortty-panel/70 px-4 py-3 text-sm text-kortty-text flex items-center justify-between gap-3">
            <div className="flex items-center gap-3">
              <Loader2 className="w-4 h-4 animate-spin text-kortty-accent" />
              <span>Waiting for AI response...</span>
            </div>
            {waitingMeta && (
              <div className="text-right text-xs text-kortty-text-dim">
                <div>Since {waitingMeta.startedAtLabel}</div>
                <div>Elapsed {waitingMeta.elapsedLabel}</div>
              </div>
            )}
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      <div className="border-t border-kortty-border bg-kortty-surface px-4 py-3 space-y-2">
        <div className="text-xs text-kortty-text-dim">{status}</div>
        <div className="flex items-end gap-3">
          <textarea
            ref={inputRef}
            className="input-field min-h-24 resize-y flex-1"
            style={{ fontSize: `${chatFontSizePx}px` }}
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={handleInputKeyDown}
            placeholder="Ask a new question or continue the chat"
            disabled={busy || !selectedProfileId}
          />
          <button
            className="px-4 py-2 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors flex items-center gap-2 disabled:opacity-50"
            disabled={busy || !selectedProfileId || !input.trim()}
            onClick={() => void handleSendFollowUp()}
          >
            <Send className="w-3.5 h-3.5" />
            Send
          </button>
          <button
            className="px-4 py-2 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors flex items-center gap-2 disabled:opacity-50"
            disabled={busy || !lastRequestRef.current}
            onClick={() => void handleRetryLastRequest()}
          >
            <RotateCcw className="w-3.5 h-3.5" />
            Reload
          </button>
          <button
            className="px-4 py-2 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors flex items-center gap-2 disabled:opacity-50"
            disabled={!busy || !currentRequestId}
            onClick={() => void handleCancelCurrentRequest()}
          >
            <X className="w-3.5 h-3.5" />
            Cancel
          </button>
        </div>
      </div>
      <AiCodeBlockDialog
        open={selectedCodeBlock != null}
        language={selectedCodeBlock?.language}
        code={selectedCodeBlock?.code || ""}
        fontSizePx={chatFontSizePx}
        onClose={() => setSelectedCodeBlock(null)}
      />
      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="fixed z-[120] min-w-[150px] rounded-lg border border-kortty-border bg-kortty-panel py-1 shadow-2xl"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onContextMenu={(event) => event.preventDefault()}
        >
          <button
            className="flex w-full items-center px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-40"
            onClick={() => void handleCopySelection()}
            disabled={!contextMenu.selectionText && !contextMenu.editableSelection}
          >
            Copy
          </button>
          <button
            className="flex w-full items-center px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-40"
            onClick={() => void handlePasteSelection()}
            disabled={!contextMenu.canPaste}
          >
            Paste
          </button>
          <button
            className="flex w-full items-center px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:cursor-not-allowed disabled:opacity-40"
            onClick={() => void handleCutSelection()}
            disabled={!contextMenu.canCut}
          >
            Cut
          </button>
        </div>
      )}
    </div>
  );
}
