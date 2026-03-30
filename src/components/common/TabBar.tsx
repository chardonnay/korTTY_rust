import { useState, useEffect, useRef, useCallback } from "react";
import { Bot, X, Plus, Terminal } from "lucide-react";
import type {
  AiRequestPayload,
  SavedAiChat,
  TerminalAgentPlanRequest,
  TerminalAgentPlanRunState,
  TerminalAgentRequest,
} from "../../types/ai";

const TAB_REORDER_MIME = "application/x-kortty-tab-reorder-id";

export interface Tab {
  id: string;
  kind?: "terminal" | "ai";
  label: string;
  status: "connected" | "connecting" | "disconnected";
  readOnlyMirror?: boolean;
  host?: string;
  port?: number;
  username?: string;
  connectionId?: string;
  password?: string;
  authMethod?: "Password" | "PrivateKey";
  credentialId?: string;
  sshKeyId?: string;
  privateKeyPath?: string;
  privateKeyPassphrase?: string;
  temporaryKeyContent?: string;
  temporaryKeyExpirationMinutes?: number;
  temporaryKeyPermanent?: boolean;
  connectionProtocol?: "TcpIp" | "Mosh";
  themeId?: string;
  fontFamily?: string;
  fontSize?: number;
  foregroundColor?: string;
  backgroundColor?: string;
  cursorColor?: string;
  ansiColors?: string[];
  aiChatId?: string;
  aiInitialRequest?: AiRequestPayload;
  aiSavedChat?: SavedAiChat;
  aiAgentRequest?: TerminalAgentRequest;
  aiAgentRunId?: string;
  aiAgentPlanRequest?: TerminalAgentPlanRequest;
  aiAgentPlanRunId?: string;
  aiAgentPlanInitialState?: TerminalAgentPlanRunState;
}

interface TabBarProps {
  tabs: Tab[];
  activeTab: string | null;
  onTabChange: (tabId: string) => void;
  onAddTab: () => void;
  onQuickConnect: () => void;
  onCloseTab: (tabId: string) => void;
  onDuplicateTab?: (tabId: string) => void;
  onReconnectTab?: (tabId: string) => void;
  onReorderTabs?: (draggedId: string, targetId: string) => void;
  onTabTransferDragStart?: (tab: Tab, e: React.DragEvent<HTMLDivElement>) => void;
  onTabTransferDragEnd?: (tab: Tab, e: React.DragEvent<HTMLDivElement>) => void;
  otherWindows?: { label: string; name: string }[];
  onMoveTabToWindow?: (tabId: string, targetWindowLabel: string) => void;
  onCopyTabToWindow?: (tabId: string, targetWindowLabel: string) => void;
}

interface CtxMenu {
  x: number;
  y: number;
  tabId: string;
}

export function TabBar({
  tabs, activeTab, onTabChange, onQuickConnect, onCloseTab,
  onDuplicateTab, onReconnectTab, onReorderTabs,
  onTabTransferDragStart, onTabTransferDragEnd,
  otherWindows, onMoveTabToWindow, onCopyTabToWindow,
}: TabBarProps) {
  const [ctxMenu, setCtxMenu] = useState<CtxMenu | null>(null);
  const [draggedId, setDraggedId] = useState<string | null>(null);
  const [dropTargetId, setDropTargetId] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const statusColors: Record<Tab["status"], string> = {
    connected: "bg-kortty-success",
    connecting: "bg-kortty-warning animate-pulse",
    disconnected: "bg-kortty-error",
  };

  useEffect(() => {
    if (!ctxMenu) return;
    function close(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setCtxMenu(null);
      }
    }
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [ctxMenu]);

  const handleCtxAction = useCallback((fn: () => void) => {
    fn();
    setCtxMenu(null);
  }, []);

  const contextTab = tabs.find((tab) => tab.id === ctxMenu?.tabId) ?? null;
  const isAiContextTab = (contextTab?.kind ?? "terminal") === "ai";

  return (
    <div className="flex items-center h-9 bg-kortty-surface border-b border-kortty-border overflow-x-auto">
      <div className="flex items-center gap-0.5 px-1 min-w-0">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            draggable
            onDragStart={(e) => {
              setDraggedId(tab.id);
              e.dataTransfer.effectAllowed = "move";
              e.dataTransfer.setData(TAB_REORDER_MIME, tab.id);
              onTabTransferDragStart?.(tab, e);
              if (!e.dataTransfer.getData("text/plain")) {
                e.dataTransfer.setData("text/plain", tab.id);
              }
            }}
            onDragEnd={(e) => {
              onTabTransferDragEnd?.(tab, e);
              setDraggedId(null);
              setDropTargetId(null);
            }}
            onDragOver={(e) => {
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              if (draggedId && tab.id !== draggedId) {
                setDropTargetId(tab.id);
              }
            }}
            onDragLeave={() => {
              if (dropTargetId === tab.id) setDropTargetId(null);
            }}
            onDrop={(e) => {
              e.preventDefault();
              const srcId = e.dataTransfer.getData(TAB_REORDER_MIME) || e.dataTransfer.getData("text/plain");
              if (srcId && srcId !== tab.id && onReorderTabs) {
                onReorderTabs(srcId, tab.id);
              }
              setDraggedId(null);
              setDropTargetId(null);
            }}
            className={`group flex items-center gap-1.5 px-3 py-1.5 rounded-t text-xs cursor-pointer transition-colors min-w-0 max-w-[180px] ${
              activeTab === tab.id
                ? "bg-kortty-terminal text-kortty-text border-t border-x border-kortty-border"
                : "text-kortty-text-dim hover:bg-kortty-panel hover:text-kortty-text"
            } ${draggedId === tab.id ? "opacity-40" : ""} ${
              dropTargetId === tab.id ? "ring-1 ring-kortty-accent ring-offset-1 ring-offset-kortty-surface" : ""
            }`}
            onClick={() => onTabChange(tab.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              setCtxMenu({ x: e.clientX, y: e.clientY, tabId: tab.id });
            }}
          >
            {(tab.kind ?? "terminal") === "ai" ? (
              <Bot className="w-3.5 h-3.5 flex-shrink-0" />
            ) : (
              <>
                <Terminal className="w-3.5 h-3.5 flex-shrink-0" />
                <span
                  className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${statusColors[tab.status]}`}
                />
              </>
            )}
            <span className="truncate">{tab.label}</span>
            <button
              className="ml-auto flex-shrink-0 opacity-0 group-hover:opacity-100 hover:text-kortty-error transition-opacity"
              onClick={(e) => {
                e.stopPropagation();
                onCloseTab(tab.id);
              }}
            >
              <X className="w-3 h-3" />
            </button>
          </div>
        ))}
      </div>
      <button
        className="flex items-center justify-center w-8 h-8 text-kortty-text-dim hover:text-kortty-accent hover:bg-kortty-panel rounded transition-colors flex-shrink-0"
        onClick={onQuickConnect}
        title="Quick Connect (Ctrl+K)"
      >
        <Plus className="w-4 h-4" />
      </button>

      {ctxMenu && (
        <div
          ref={menuRef}
          className="fixed z-[100] bg-kortty-panel border border-kortty-border rounded-lg shadow-2xl py-1 min-w-[160px]"
          style={{ left: ctxMenu.x, top: ctxMenu.y }}
        >
          {onDuplicateTab && !isAiContextTab && (
            <button
              className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors"
              onClick={() => handleCtxAction(() => onDuplicateTab(ctxMenu.tabId))}
            >
              Duplicate Tab
            </button>
          )}
          {onReconnectTab && !isAiContextTab && (() => {
            const t = tabs.find((tab) => tab.id === ctxMenu.tabId);
            return t?.host ? (
              <button
                className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors"
                onClick={() => handleCtxAction(() => onReconnectTab(ctxMenu.tabId))}
              >
                Reconnect
              </button>
            ) : null;
          })()}
          {!isAiContextTab && otherWindows && otherWindows.length > 0 && (onMoveTabToWindow || onCopyTabToWindow) && (
            <>
              <div className="my-1 border-t border-kortty-border" />
              {onMoveTabToWindow && (
                <>
                  <div className="px-2 py-1 text-[10px] text-kortty-text-dim uppercase">Move to window</div>
                  {otherWindows.map((win) => (
                    <button
                      key={win.label}
                      className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors text-left"
                      onClick={() => handleCtxAction(() => onMoveTabToWindow(ctxMenu.tabId, win.label))}
                    >
                      {win.name}
                    </button>
                  ))}
                </>
              )}
              {onCopyTabToWindow && (
                <>
                  <div className="px-2 py-1 text-[10px] text-kortty-text-dim uppercase">Copy to window</div>
                  {otherWindows.map((win) => (
                    <button
                      key={`copy-${win.label}`}
                      className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors text-left"
                      onClick={() => handleCtxAction(() => onCopyTabToWindow(ctxMenu.tabId, win.label))}
                    >
                      {win.name}
                    </button>
                  ))}
                </>
              )}
            </>
          )}
          <div className="my-1 border-t border-kortty-border" />
          <button
            className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors"
            onClick={() => handleCtxAction(() => onCloseTab(ctxMenu.tabId))}
          >
            Close Tab
          </button>
          <button
            className="w-full flex items-center px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors"
            onClick={() => handleCtxAction(() => {
              const keepId = activeTab ?? ctxMenu.tabId;
              tabs.filter((t) => t.id !== keepId).forEach((t) => onCloseTab(t.id));
            })}
            disabled={tabs.length <= 1}
          >
            Close Other Tabs
          </button>
        </div>
      )}
    </div>
  );
}
