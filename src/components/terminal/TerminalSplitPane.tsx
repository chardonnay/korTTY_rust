import { useState, useCallback } from "react";
import { Panel, PanelGroup, PanelResizeHandle } from "react-resizable-panels";
import { TerminalTab } from "./TerminalTab";
import { Radio, SplitSquareVertical, SplitSquareHorizontal, X } from "lucide-react";

interface SplitSession {
  id: string;
  sessionId: string;
  connected: boolean;
}

interface TerminalSplitPaneProps {
  primarySessionId: string;
  connected: boolean;
}

export function TerminalSplitPane({ primarySessionId, connected }: TerminalSplitPaneProps) {
  const [splits, setSplits] = useState<SplitSession[]>([
    { id: "primary", sessionId: primarySessionId, connected },
  ]);
  const [direction, setDirection] = useState<"horizontal" | "vertical">("horizontal");
  const [broadcast, setBroadcast] = useState(false);

  const addSplit = useCallback(() => {
    const newSplit: SplitSession = {
      id: crypto.randomUUID(),
      sessionId: primarySessionId,
      connected,
    };
    setSplits((prev) => [...prev, newSplit]);
  }, [primarySessionId, connected]);

  const removeSplit = useCallback((id: string) => {
    setSplits((prev) => {
      if (prev.length <= 1) return prev;
      return prev.filter((s) => s.id !== id);
    });
  }, []);

  return (
    <div className="flex flex-col w-full h-full">
      <div className="flex items-center gap-1 px-2 py-0.5 bg-kortty-surface border-b border-kortty-border">
        <button
          className={`p-1 rounded text-xs transition-colors ${
            broadcast ? "text-kortty-warning bg-kortty-warning/10" : "text-kortty-text-dim hover:text-kortty-text"
          }`}
          onClick={() => setBroadcast(!broadcast)}
          title="Broadcast mode - send input to all panes"
        >
          <Radio className="w-3.5 h-3.5" />
        </button>
        <button
          className="p-1 rounded text-kortty-text-dim hover:text-kortty-text transition-colors"
          onClick={() => setDirection("horizontal")}
          title="Horizontal split"
        >
          <SplitSquareVertical className="w-3.5 h-3.5" />
        </button>
        <button
          className="p-1 rounded text-kortty-text-dim hover:text-kortty-text transition-colors"
          onClick={() => setDirection("vertical")}
          title="Vertical split"
        >
          <SplitSquareHorizontal className="w-3.5 h-3.5" />
        </button>
        <button
          className="p-1 rounded text-xs text-kortty-text-dim hover:text-kortty-accent transition-colors"
          onClick={addSplit}
          title="Add split pane"
        >
          +
        </button>
        {broadcast && (
          <span className="text-[10px] text-kortty-warning ml-1">BROADCAST</span>
        )}
      </div>
      <div className="flex-1">
        <PanelGroup direction={direction}>
          {splits.map((split, idx) => (
            <div key={split.id} className="contents">
              {idx > 0 && (
                <PanelResizeHandle className="w-1 bg-kortty-border hover:bg-kortty-accent transition-colors cursor-col-resize" />
              )}
              <Panel minSize={10}>
                <div className="relative w-full h-full group">
                  <TerminalTab sessionId={split.sessionId} connected={split.connected} />
                  {splits.length > 1 && (
                    <button
                      className="absolute top-1 right-1 p-0.5 bg-kortty-surface/80 rounded opacity-0 group-hover:opacity-100 transition-opacity text-kortty-text-dim hover:text-kortty-error"
                      onClick={() => removeSplit(split.id)}
                    >
                      <X className="w-3 h-3" />
                    </button>
                  )}
                </div>
              </Panel>
            </div>
          ))}
        </PanelGroup>
      </div>
    </div>
  );
}
