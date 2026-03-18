import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { Bot, Copy, Download, Loader2, Save, Send, Wand2 } from "lucide-react";
import type {
  AiExecutionResult,
  AiProfile,
  AiRequestPayload,
  AiTokenUsageSnapshot,
  SavedAiChat,
  SavedAiChatMessage,
} from "../../types/ai";

interface AiChatTabProps {
  tabId: string;
  initialRequest?: AiRequestPayload;
  initialChat?: SavedAiChat;
  onTitleChange: (title: string) => void;
  onSavedChatIdChange: (savedChatId: string) => void;
}

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

export function AiChatTab({
  tabId,
  initialRequest,
  initialChat,
  onTitleChange,
  onSavedChatIdChange,
}: AiChatTabProps) {
  const [profiles, setProfiles] = useState<AiProfile[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState(
    initialChat?.activeAiProfileId || initialRequest?.profileId || "",
  );
  const [messages, setMessages] = useState<SavedAiChatMessage[]>(initialChat?.messages || []);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string>("Ready");
  const [savedChatId, setSavedChatId] = useState(initialChat?.id || "");
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
  const [responseLanguageCode] = useState(initialChat?.responseLanguageCode || initialRequest?.responseLanguageCode || "en");
  const [usageSnapshot, setUsageSnapshot] = useState<AiTokenUsageSnapshot | undefined>();
  const initialRunRef = useRef(false);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);

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
    setBusy(true);
    setStatus("Waiting for AI response...");
    const trimmedUserPrompt = request.userPrompt?.trim();
    if (appendUserPrompt && trimmedUserPrompt) {
      setMessages((current) => [
        ...current,
        createMessage("User", trimmedUserPrompt),
      ]);
    }

    try {
      const result = await invoke<AiExecutionResult>("execute_ai_action", { request });
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
      setMessages((current) => [
        ...current,
        createMessage("Assistant", `AI request failed: ${String(error)}`),
      ]);
      setStatus("AI request failed.");
    } finally {
      setBusy(false);
    }
  }, [loadProfiles]);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

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

  async function handleCopyTranscript() {
    try {
      await navigator.clipboard.writeText(buildPlainTranscript(messages));
      setStatus("Transcript copied to clipboard.");
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
    const nextTitle = window.prompt("Chat title", title || "AI Chat");
    if (!nextTitle?.trim()) return;

    try {
      const saved = await invoke<SavedAiChat>("save_ai_chat", {
        chat: {
          id: savedChatId,
          title: nextTitle.trim(),
          createdAt: initialChat?.createdAt || Date.now(),
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
      onSavedChatIdChange(saved.id);
      setStatus(`Saved chat "${saved.title}".`);
    } catch (error) {
      setStatus(`Save failed: ${String(error)}`);
    }
  }

  return (
    <div className="flex flex-col h-full min-h-0 bg-kortty-terminal">
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
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
        {messages.length === 0 && !busy && (
          <div className="h-full flex items-center justify-center text-sm text-kortty-text-dim">
            No AI messages yet.
          </div>
        )}
        {messages.map((message, index) => (
          <div
            key={`${tabId}-${index}-${message.createdAt}`}
            className={`rounded-lg border px-4 py-3 ${
              message.role === "User"
                ? "bg-kortty-surface border-kortty-border"
                : "bg-kortty-panel/70 border-kortty-accent/30"
            }`}
          >
            <div className="flex items-center gap-2 mb-2 text-[11px] text-kortty-text-dim">
              <Bot className="w-3.5 h-3.5" />
              <span>{message.role === "User" ? "User" : message.aiProfileName || "Assistant"}</span>
              <span>{new Date(message.createdAt).toLocaleTimeString()}</span>
            </div>
            <pre className="whitespace-pre-wrap break-words text-sm text-kortty-text font-mono leading-6">
              {message.content}
            </pre>
          </div>
        ))}
        {busy && (
          <div className="rounded-lg border border-kortty-accent/30 bg-kortty-panel/70 px-4 py-3 text-sm text-kortty-text flex items-center gap-3">
            <Loader2 className="w-4 h-4 animate-spin text-kortty-accent" />
            Waiting for AI response...
          </div>
        )}
        <div ref={messagesEndRef} />
      </div>

      <div className="border-t border-kortty-border bg-kortty-surface px-4 py-3 space-y-2">
        <div className="text-xs text-kortty-text-dim">{status}</div>
        <div className="flex items-end gap-3">
          <textarea
            className="input-field min-h-24 resize-y flex-1"
            value={input}
            onChange={(event) => setInput(event.target.value)}
            placeholder="Continue the conversation about the selected terminal text"
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
        </div>
      </div>
    </div>
  );
}
