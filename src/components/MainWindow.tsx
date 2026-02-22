import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MenuBar } from "./common/MenuBar";
import { TabBar, Tab } from "./common/TabBar";
import { StatusBar } from "./common/StatusBar";
import { TerminalTab } from "./terminal/TerminalTab";

export function MainWindow() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTab, setActiveTab] = useState<string | null>(null);
  const [showDashboard, setShowDashboard] = useState(false);
  const [connectionCount, setConnectionCount] = useState(0);

  useEffect(() => {
    setConnectionCount(tabs.filter((t) => t.status === "connected").length);
  }, [tabs]);

  const handleConnect = useCallback(
    async (tabId: string, host: string, port: number, username: string, password: string) => {
      setTabs((prev) =>
        prev.map((t) =>
          t.id === tabId ? { ...t, status: "connecting" as const } : t,
        ),
      );
      try {
        await invoke("ssh_connect", {
          sessionId: tabId,
          settings: {
            id: tabId,
            name: `${username}@${host}`,
            host,
            port,
            username,
            authMethod: "Password",
            password,
            fontFamily: "JetBrains Mono",
            fontSize: 14.0,
            columns: 80,
            rows: 24,
            scrollbackLines: 10000,
            foregroundColor: "#cdd6f4",
            backgroundColor: "#11111b",
            cursorColor: "#89b4fa",
            cursorStyle: "Block",
            ansiColors: [
              "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
              "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
              "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
              "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
            ],
            sshKeepaliveEnabled: true,
            sshKeepaliveInterval: 60,
            connectionTimeout: 15,
            retryCount: 4,
            terminalLogging: false,
            commandTimestamps: false,
            tunnels: [],
            usageCount: 0,
          },
        });
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId
              ? { ...t, label: `${username}@${host}`, status: "connected" as const }
              : t,
          ),
        );
      } catch (err) {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId ? { ...t, status: "disconnected" as const } : t,
          ),
        );
        console.error("Connection failed:", err);
      }
    },
    [],
  );

  const handleDisconnect = useCallback(async (tabId: string) => {
    try {
      await invoke("ssh_disconnect", { sessionId: tabId });
    } catch (err) {
      console.error("Disconnect failed:", err);
    }
    setTabs((prev) =>
      prev.map((t) =>
        t.id === tabId ? { ...t, status: "disconnected" as const } : t,
      ),
    );
  }, []);

  const addTab = useCallback(() => {
    const id = crypto.randomUUID();
    const newTab: Tab = { id, label: "New Connection", status: "disconnected" };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
  }, []);

  const closeTab = useCallback(
    (id: string) => {
      const tab = tabs.find((t) => t.id === id);
      if (tab?.status === "connected") {
        handleDisconnect(id);
      }
      setTabs((prev) => {
        const remaining = prev.filter((t) => t.id !== id);
        if (activeTab === id) {
          setActiveTab(remaining.length > 0 ? remaining[remaining.length - 1].id : null);
        }
        return remaining;
      });
    },
    [tabs, activeTab, handleDisconnect],
  );

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.ctrlKey && e.key === "t") {
        e.preventDefault();
        addTab();
      } else if (e.ctrlKey && e.key === "w") {
        e.preventDefault();
        if (activeTab) closeTab(activeTab);
      } else if (e.ctrlKey && e.shiftKey && e.key === "D") {
        e.preventDefault();
        setShowDashboard((prev) => !prev);
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [addTab, closeTab, activeTab]);

  const currentTab = tabs.find((t) => t.id === activeTab);

  return (
    <div className="flex flex-col h-screen w-screen bg-kortty-bg">
      <MenuBar
        onToggleDashboard={() => setShowDashboard(!showDashboard)}
        onNewTab={addTab}
        onConnect={handleConnect}
        activeTabId={activeTab}
      />
      <div className="flex flex-1 min-h-0">
        {showDashboard && (
          <div className="w-[300px] border-r border-kortty-border bg-kortty-surface flex-shrink-0 flex flex-col">
            <div className="px-3 py-2 text-xs font-semibold text-kortty-text-dim uppercase tracking-wider border-b border-kortty-border">
              Dashboard
            </div>
            <div className="flex-1 p-3 text-sm">
              {tabs.length === 0 ? (
                <div className="text-kortty-text-dim">No active connections</div>
              ) : (
                <div className="space-y-1">
                  {tabs.map((tab) => (
                    <div
                      key={tab.id}
                      className={`px-2 py-1.5 rounded text-xs cursor-pointer transition-colors ${
                        activeTab === tab.id
                          ? "bg-kortty-accent/10 text-kortty-accent"
                          : "text-kortty-text hover:bg-kortty-panel"
                      }`}
                      onClick={() => setActiveTab(tab.id)}
                    >
                      <div className="flex items-center gap-2">
                        <span
                          className={`w-2 h-2 rounded-full ${
                            tab.status === "connected"
                              ? "bg-kortty-success"
                              : tab.status === "connecting"
                                ? "bg-kortty-warning"
                                : "bg-kortty-error"
                          }`}
                        />
                        <span className="truncate">{tab.label}</span>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}
        <div className="flex-1 flex flex-col min-w-0">
          <TabBar
            tabs={tabs}
            activeTab={activeTab}
            onTabChange={setActiveTab}
            onAddTab={addTab}
            onCloseTab={closeTab}
          />
          <div className="flex-1 bg-kortty-terminal relative">
            {tabs.map((tab) => (
              <div
                key={tab.id}
                className="absolute inset-0"
                style={{ display: tab.id === activeTab ? "block" : "none" }}
              >
                {tab.status === "connected" ? (
                  <TerminalTab sessionId={tab.id} connected={true} />
                ) : (
                  <div className="w-full h-full flex items-center justify-center">
                    <div className="text-center text-kortty-text-dim text-sm">
                      {tab.status === "connecting" ? (
                        <div className="flex items-center gap-2">
                          <div className="animate-spin w-4 h-4 border-2 border-kortty-accent border-t-transparent rounded-full" />
                          Connecting...
                        </div>
                      ) : (
                        <p>Not connected. Use the connection manager to connect.</p>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
            {tabs.length === 0 && (
              <div className="w-full h-full flex items-center justify-center">
                <div className="text-center text-kortty-text-dim">
                  <div className="text-6xl mb-4 font-mono font-bold text-kortty-accent opacity-30">
                    KorTTY
                  </div>
                  <p className="text-sm">
                    Press{" "}
                    <kbd className="px-1.5 py-0.5 bg-kortty-panel rounded text-xs font-mono">
                      Ctrl+T
                    </kbd>{" "}
                    to open a new tab or{" "}
                    <kbd className="px-1.5 py-0.5 bg-kortty-panel rounded text-xs font-mono">
                      Ctrl+K
                    </kbd>{" "}
                    for Quick Connect
                  </p>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
      <StatusBar connectionCount={connectionCount} />
    </div>
  );
}
