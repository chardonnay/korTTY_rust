import { X, Plus, Terminal } from "lucide-react";

export interface Tab {
  id: string;
  label: string;
  status: "connected" | "connecting" | "disconnected";
}

interface TabBarProps {
  tabs: Tab[];
  activeTab: string | null;
  onTabChange: (tabId: string) => void;
  onAddTab: () => void;
  onCloseTab: (tabId: string) => void;
}

export function TabBar({ tabs, activeTab, onTabChange, onAddTab, onCloseTab }: TabBarProps) {
  const statusColors: Record<Tab["status"], string> = {
    connected: "bg-kortty-success",
    connecting: "bg-kortty-warning animate-pulse",
    disconnected: "bg-kortty-error",
  };

  return (
    <div className="flex items-center h-9 bg-kortty-surface border-b border-kortty-border overflow-x-auto">
      <div className="flex items-center gap-0.5 px-1 min-w-0">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`group flex items-center gap-1.5 px-3 py-1.5 rounded-t text-xs cursor-pointer transition-colors min-w-0 max-w-[180px] ${
              activeTab === tab.id
                ? "bg-kortty-terminal text-kortty-text border-t border-x border-kortty-border"
                : "text-kortty-text-dim hover:bg-kortty-panel hover:text-kortty-text"
            }`}
            onClick={() => onTabChange(tab.id)}
          >
            <Terminal className="w-3.5 h-3.5 flex-shrink-0" />
            <span
              className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${statusColors[tab.status]}`}
            />
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
        onClick={onAddTab}
        title="New Tab (Ctrl+T)"
      >
        <Plus className="w-4 h-4" />
      </button>
    </div>
  );
}
