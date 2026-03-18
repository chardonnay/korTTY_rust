import { useState, useCallback, useEffect, useLayoutEffect, useRef, useMemo, Fragment } from "react";
import { createPortal } from "react-dom";
import { Panel, PanelGroup, PanelResizeHandle, type ImperativePanelGroupHandle } from "react-resizable-panels";
import { TerminalTab } from "./TerminalTab";
import { Radio, X, ChevronRight, GripVertical } from "lucide-react";
import type { AiAction } from "../../types/ai";

// --- Split tree data model ---

interface LeafNode {
  type: "leaf";
  id: string;
  sessionId: string;
  connected: boolean;
}

interface ContainerNode {
  type: "container";
  id: string;
  direction: "horizontal" | "vertical";
  children: SplitNode[];
}

export type SplitNode = LeafNode | ContainerNode;

/** Serializable split tree for cross-window transfer (structure + sessionIds only). */
export type SplitTreeTransferNode =
  | { type: "leaf"; sessionId: string }
  | { type: "container"; direction: "horizontal" | "vertical"; children: SplitTreeTransferNode[] };

export function serializeSplitTree(tree: SplitNode): SplitTreeTransferNode {
  if (tree.type === "leaf") {
    return { type: "leaf", sessionId: tree.sessionId };
  }
  return {
    type: "container",
    direction: tree.direction,
    children: tree.children.map(serializeSplitTree),
  };
}

/** Returns session IDs in display order (depth-first). */
export function getLeavesInOrder(transfer: SplitTreeTransferNode): string[] {
  if (transfer.type === "leaf") return [transfer.sessionId];
  return transfer.children.flatMap(getLeavesInOrder);
}

let _leafIdCounter = 0;
function nextLeafId(): string {
  if (_leafIdCounter === 0) return "primary";
  return `split-${_leafIdCounter++}`;
}

function deserializeSplitTree(
  transfer: SplitTreeTransferNode,
  sessionIdMap: Record<string, string>,
): SplitNode {
  if (transfer.type === "leaf") {
    const newSessionId = sessionIdMap[transfer.sessionId] ?? transfer.sessionId;
    return { type: "leaf", id: nextLeafId(), sessionId: newSessionId, connected: true };
  }
  return {
    type: "container",
    id: crypto.randomUUID(),
    direction: transfer.direction,
    children: transfer.children.map((c) => deserializeSplitTree(c, sessionIdMap)),
  };
}

/** Deserialize transfer tree with sessionId mapping; first leaf gets id "primary". */
export function deserializeSplitTreeWithMapping(
  transfer: SplitTreeTransferNode,
  sessionIdMap: Record<string, string>,
): SplitNode {
  _leafIdCounter = 0;
  if (transfer.type === "leaf") {
    const newSessionId = sessionIdMap[transfer.sessionId] ?? transfer.sessionId;
    return { type: "leaf", id: "primary", sessionId: newSessionId, connected: true };
  }
  return {
    type: "container",
    id: "root",
    direction: transfer.direction,
    children: transfer.children.map((c) => deserializeSplitTree(c, sessionIdMap)),
  };
}

function getAllLeaves(node: SplitNode): LeafNode[] {
  if (node.type === "leaf") return [node];
  return node.children.flatMap(getAllLeaves);
}

function splitLeafInTree(
  node: SplitNode,
  leafId: string,
  direction: "horizontal" | "vertical",
  newLeaf: LeafNode,
): SplitNode {
  if (node.type === "leaf") {
    if (node.id === leafId) {
      return {
        type: "container",
        id: crypto.randomUUID(),
        direction,
        children: [node, newLeaf],
      };
    }
    return node;
  }

  const childIdx = node.children.findIndex(
    (c) => c.type === "leaf" && c.id === leafId,
  );
  if (childIdx >= 0 && node.direction === direction) {
    const newChildren = [...node.children];
    newChildren.splice(childIdx + 1, 0, newLeaf);
    return { ...node, children: newChildren };
  }

  return {
    ...node,
    children: node.children.map((c) =>
      splitLeafInTree(c, leafId, direction, newLeaf),
    ),
  };
}

function removeLeafFromTree(node: SplitNode, leafId: string): SplitNode | null {
  if (node.type === "leaf") {
    return node.id === leafId ? null : node;
  }
  const remaining = node.children
    .map((c) => removeLeafFromTree(c, leafId))
    .filter((c): c is SplitNode => c !== null);
  if (remaining.length === 0) return null;
  if (remaining.length === 1) return remaining[0];
  return { ...node, children: remaining };
}

function updateLeafConnected(
  node: SplitNode,
  sessionId: string,
  connected: boolean,
): SplitNode {
  if (node.type === "leaf") {
    return node.sessionId === sessionId ? { ...node, connected } : node;
  }
  return {
    ...node,
    children: node.children.map((c) =>
      updateLeafConnected(c, sessionId, connected),
    ),
  };
}

function swapLeafSessions(
  node: SplitNode,
  idA: string,
  idB: string,
  dataA: { sessionId: string; connected: boolean },
  dataB: { sessionId: string; connected: boolean },
): SplitNode {
  if (node.type === "leaf") {
    // Keep leaf identity (id/position) stable and only swap session payload.
    if (node.id === idA) return { ...node, sessionId: dataB.sessionId, connected: dataB.connected };
    if (node.id === idB) return { ...node, sessionId: dataA.sessionId, connected: dataA.connected };
    return node;
  }
  return {
    ...node,
    children: node.children.map((c) => swapLeafSessions(c, idA, idB, dataA, dataB)),
  };
}

// --- Component interfaces ---

interface ContextMenuState {
  x: number;
  y: number;
  leafId: string;
  sessionId: string;
  selectedText: string;
}

interface TerminalTheme {
  foreground: string;
  background: string;
  cursor: string;
  selectionBackground: string;
  ansiColors: string[];
}

interface TerminalSplitPaneProps {
  primarySessionId: string;
  connected: boolean;
  readOnly?: boolean;
  promptHookEnabled?: boolean;
  fontSize: number;
  getFontSizeForSession?: (sessionId: string) => number;
  theme?: TerminalTheme;
  fontFamily?: string;
  onZoomIn: (sessionId: string) => void;
  onZoomOut: (sessionId: string) => void;
  onResetZoom: (sessionId: string) => void;
  onFocusSession?: (sessionId: string) => void;
  onToggleTimestamps: () => void;
  showTimestamps: boolean;
  onReconnect: (sessionId: string) => void;
  onAiAction?: (sessionId: string, action: AiAction, selectedText: string) => void;
  onClosePrimarySplit?: () => void;
  onCloseRequest?: () => void;
  onSplitSameServer: () => Promise<string | null>;
  onSplitNewServer: () => Promise<string | null>;
  onDisconnectSplitSession?: (sessionId: string) => void;
  initialSplitSessionIds?: string[];
  /** Restore exact split layout (e.g. after cross-window transfer). Takes precedence over initialSplitSessionIds. */
  initialTree?: SplitNode;
  /** Called when the split tree changes (for persisting layout, e.g. transfer). */
  onTreeChange?: (tree: SplitNode) => void;
}

export function TerminalSplitPane({
  primarySessionId,
  connected,
  readOnly = false,
  promptHookEnabled = true,
  fontSize,
  getFontSizeForSession,
  theme,
  fontFamily,
  onZoomIn,
  onZoomOut,
  onResetZoom,
  onFocusSession,
  onToggleTimestamps,
  showTimestamps,
  onReconnect,
  onAiAction,
  onClosePrimarySplit,
  onCloseRequest,
  onSplitSameServer,
  onSplitNewServer,
  onDisconnectSplitSession,
  initialSplitSessionIds,
  initialTree,
  onTreeChange,
}: TerminalSplitPaneProps) {
  const [tree, setTree] = useState<SplitNode>(() => {
    if (initialTree) {
      return initialTree;
    }
    return {
      type: "container",
      id: "root",
      direction: "horizontal",
      children: [
        { type: "leaf", id: "primary", sessionId: primarySessionId, connected },
      ],
    };
  });
  const hasAppliedInitialSplits = useRef(!!initialTree);
  const [broadcast, setBroadcast] = useState(false);

  useEffect(() => {
    onTreeChange?.(tree);
  }, [tree, onTreeChange]);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [swapDrag, setSwapDrag] = useState<{ sourceId: string; targetId: string | null } | null>(null);
  const [ctrlShiftHeld, setCtrlShiftHeld] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const panelGroupRefs = useRef<Map<string, ImperativePanelGroupHandle>>(new Map());
  const hostElementsRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const slotRefs = useRef<Map<string, HTMLDivElement | null>>(new Map());
  const treeRef = useRef(tree);
  treeRef.current = tree;
  const swapDragRef = useRef(swapDrag);
  swapDragRef.current = swapDrag;

  useEffect(() => {
    setTree((prev) => updateLeafConnected(prev, primarySessionId, connected));
  }, [connected, primarySessionId]);

  useEffect(() => {
    if (
      hasAppliedInitialSplits.current ||
      !initialSplitSessionIds ||
      initialSplitSessionIds.length === 0
    ) {
      return;
    }
    const leaves = getAllLeaves(tree);
    if (leaves.length !== 1 || leaves[0].id !== "primary") {
      return;
    }
    hasAppliedInitialSplits.current = true;
    setTree({
      type: "container",
      id: "root",
      direction: "horizontal",
      children: [
        { type: "leaf", id: "primary", sessionId: primarySessionId, connected },
        ...initialSplitSessionIds.map((sessionId, i) => ({
          type: "leaf" as const,
          id: `split-${i}`,
          sessionId,
          connected: true,
        })),
      ],
    });
  }, [tree, primarySessionId, connected, initialSplitSessionIds]);

  const allLeaves = useMemo(() => getAllLeaves(tree), [tree]);

  // Ensure stable host elements exist for each session (created during render for portal targets)
  for (const leaf of allLeaves) {
    if (!hostElementsRef.current.has(leaf.sessionId)) {
      const el = document.createElement("div");
      el.style.position = "absolute";
      el.style.inset = "0";
      el.style.overflow = "hidden";
      hostElementsRef.current.set(leaf.sessionId, el);
    }
  }

  const leafSessionKey = allLeaves.map((l) => `${l.id}:${l.sessionId}`).join("|");

  // Place host elements in correct slots (before browser paint to avoid flicker)
  useLayoutEffect(() => {
    for (const leaf of getAllLeaves(treeRef.current)) {
      const host = hostElementsRef.current.get(leaf.sessionId);
      const slot = slotRefs.current.get(leaf.id);
      if (host && slot && host.parentElement !== slot) {
        slot.appendChild(host);
        window.dispatchEvent(
          new CustomEvent("kortty-terminal-reattach", {
            detail: { sessionId: leaf.sessionId },
          }),
        );
      }
    }
    // Refit is handled per reattached terminal to avoid global redraw/reset effects on moves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [leafSessionKey]);

  // Cleanup host elements for removed sessions
  useEffect(() => {
    const currentIds = new Set(getAllLeaves(treeRef.current).map((l) => l.sessionId));
    for (const [sid, el] of hostElementsRef.current) {
      if (!currentIds.has(sid)) {
        el.remove();
        hostElementsRef.current.delete(sid);
      }
    }
  }, [allLeaves]);

  // Cleanup all hosts on unmount
  useEffect(() => {
    return () => {
      for (const [, el] of hostElementsRef.current) {
        el.remove();
      }
      hostElementsRef.current.clear();
    };
  }, []);

  const prevLeafCount = useRef(allLeaves.length);
  useEffect(() => {
    if (allLeaves.length === prevLeafCount.current) return;
    prevLeafCount.current = allLeaves.length;

    function equalizeAll(node: SplitNode) {
      if (node.type !== "container" || node.children.length < 2) return;
      const handle = panelGroupRefs.current.get(node.id);
      if (handle) {
        try {
          handle.setLayout(node.children.map(() => 100 / node.children.length));
        } catch { /* not mounted yet */ }
      }
      node.children.forEach(equalizeAll);
    }

    requestAnimationFrame(() => equalizeAll(treeRef.current));

    const t1 = setTimeout(() => window.dispatchEvent(new Event("kortty-refit")), 50);
    const t2 = setTimeout(() => window.dispatchEvent(new Event("kortty-refit")), 150);
    const t3 = setTimeout(() => window.dispatchEvent(new Event("kortty-refit")), 400);
    return () => { clearTimeout(t1); clearTimeout(t2); clearTimeout(t3); };
  }, [allLeaves.length]);

  const handleSplit = useCallback(
    async (dir: "horizontal" | "vertical", mode: "same" | "new") => {
      const targetLeafId = contextMenu?.leafId;
      setContextMenu(null);
      if (!targetLeafId) return;

      const sessionId =
        mode === "same" ? await onSplitSameServer() : await onSplitNewServer();
      if (sessionId) {
        const newLeaf: LeafNode = {
          type: "leaf",
          id: crypto.randomUUID(),
          sessionId,
          connected: true,
        };
        setTree((prev) => splitLeafInTree(prev, targetLeafId, dir, newLeaf));
      }
    },
    [contextMenu, onSplitSameServer, onSplitNewServer],
  );

  const removeSplit = useCallback(
    (leafId: string) => {
      const leaves = getAllLeaves(tree);
      if (leaves.length <= 1) return;
      const leaf = leaves.find((l) => l.id === leafId);
      if (leaf && leaf.sessionId !== primarySessionId && onDisconnectSplitSession) {
        onDisconnectSplitSession(leaf.sessionId);
      }
      setTree((prev) => {
        const result = removeLeafFromTree(prev, leafId);
        if (!result) return prev;
        if (result.type === "leaf") {
          return { type: "container", id: "root", direction: "horizontal", children: [result] };
        }
        return result;
      });
    },
    [tree, primarySessionId, onDisconnectSplitSession],
  );

  useEffect(() => {
    function handleRemoveSplitSession(event: Event) {
      const custom = event as CustomEvent<{ sessionId: string }>;
      const sessionId = custom.detail?.sessionId;
      if (!sessionId) return;
      const leaf = getAllLeaves(treeRef.current).find((l) => l.sessionId === sessionId && l.id !== "primary");
      if (!leaf) return;
      removeSplit(leaf.id);
    }

    window.addEventListener("kortty-remove-split-session", handleRemoveSplitSession as EventListener);
    return () => {
      window.removeEventListener("kortty-remove-split-session", handleRemoveSplitSession as EventListener);
    };
  }, [removeSplit]);

  const openContextMenu = useCallback((e: React.MouseEvent, leafId: string, sessionId: string, selectedText = "") => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY, leafId, sessionId, selectedText });
  }, []);

  useEffect(() => {
    if (!contextMenu) return;
    function handleClick(e: MouseEvent) {
      if (menuRef.current && menuRef.current.contains(e.target as Node)) return;
      setContextMenu(null);
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [contextMenu]);

  const menuAction = useCallback((fn: () => void) => {
    fn();
    setContextMenu(null);
  }, []);

  const currentContextLeaf = useMemo(
    () => allLeaves.find((l) => l.id === contextMenu?.leafId) ?? null,
    [allLeaves, contextMenu],
  );

  const triggerTerminalAction = useCallback((action: "copy" | "paste") => {
    if (!currentContextLeaf) return;
    window.dispatchEvent(
      new CustomEvent(`kortty-terminal-${action}`, {
        detail: { sessionId: currentContextLeaf.sessionId },
      }),
    );
  }, [currentContextLeaf]);

  const broadcastTargets = useMemo(() => {
    if (!broadcast || allLeaves.length < 2) return {};
    const result: Record<string, string[]> = {};
    for (const leaf of allLeaves) {
      result[leaf.id] = allLeaves
        .filter((l) => l.id !== leaf.id && l.connected)
        .map((l) => l.sessionId);
    }
    return result;
  }, [broadcast, allLeaves]);

  const leafBySessionId = useMemo(() => {
    const map = new Map<string, LeafNode>();
    for (const leaf of allLeaves) {
      map.set(leaf.sessionId, leaf);
    }
    return map;
  }, [allLeaves]);

  const broadcastTargetsBySessionId = useMemo(() => {
    if (!broadcast || allLeaves.length < 2) return {};
    const result: Record<string, string[]> = {};
    for (const leaf of allLeaves) {
      result[leaf.sessionId] = allLeaves
        .filter((l) => l.sessionId !== leaf.sessionId && l.connected)
        .map((l) => l.sessionId);
    }
    return result;
  }, [broadcast, allLeaves]);

  const activeSessionIds = useMemo(
    () => [...allLeaves.map((l) => l.sessionId)].sort(),
    [allLeaves],
  );

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      setCtrlShiftHeld(e.altKey && e.shiftKey && !e.metaKey && !e.ctrlKey);
    }
    function onBlur() {
      setCtrlShiftHeld(false);
      setSwapDrag(null);
    }
    window.addEventListener("keydown", onKey);
    window.addEventListener("keyup", onKey);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keyup", onKey);
      window.removeEventListener("blur", onBlur);
    };
  }, []);

  useEffect(() => {
    if (!swapDrag) return;
    function handleMouseUp() {
      const drag = swapDragRef.current;
      if (drag?.sourceId && drag?.targetId) {
        const leaves = getAllLeaves(treeRef.current);
        const leafA = leaves.find((l) => l.id === drag.sourceId);
        const leafB = leaves.find((l) => l.id === drag.targetId);
        if (leafA && leafB) {
          setTree((prev) =>
            swapLeafSessions(
              prev, leafA.id, leafB.id,
              { sessionId: leafA.sessionId, connected: leafA.connected },
              { sessionId: leafB.sessionId, connected: leafB.connected },
            ),
          );
        }
      }
      setSwapDrag(null);
    }
    function handleKeyUp(e: KeyboardEvent) {
      if (!e.altKey || !e.shiftKey) {
        setSwapDrag(null);
        setCtrlShiftHeld(false);
      }
    }
    document.addEventListener("mouseup", handleMouseUp);
    document.addEventListener("keyup", handleKeyUp);
    return () => {
      document.removeEventListener("mouseup", handleMouseUp);
      document.removeEventListener("keyup", handleKeyUp);
    };
  }, [swapDrag]);

  // Document-level focus capture so we always know which pane has focus (works with portaled content)
  useEffect(() => {
    if (!onFocusSession) return;
    function handleFocusIn(e: FocusEvent) {
      const target = e.target as Node;
      const pane = (target as Element).closest?.("[data-pane-session-id]");
      if (pane) {
        const sessionId = (pane as HTMLElement).getAttribute("data-pane-session-id");
        if (sessionId) onFocusSession?.(sessionId);
      }
    }
    document.addEventListener("focusin", handleFocusIn, true);
    return () => document.removeEventListener("focusin", handleFocusIn, true);
  }, [onFocusSession]);

  function renderNode(node: SplitNode): React.ReactNode {
    if (node.type === "leaf") {
      return (
        <div
          className="relative w-full h-full min-h-0 min-w-0 group overflow-hidden"
          onContextMenu={(e) => openContextMenu(e, node.id, node.sessionId)}
          data-pane-session-id={node.sessionId}
        >
          <div
            ref={(el) => {
              if (el) {
                slotRefs.current.set(node.id, el);
                const host = hostElementsRef.current.get(node.sessionId);
                if (host && host.parentElement !== el) {
                  el.appendChild(host);
                  window.dispatchEvent(
                    new CustomEvent("kortty-terminal-reattach", {
                      detail: { sessionId: node.sessionId },
                    }),
                  );
                }
              } else {
                slotRefs.current.delete(node.id);
              }
            }}
            className="absolute inset-0 overflow-hidden"
          />
          {allLeaves.length > 1 && !swapDrag && (
            <button
              className="absolute top-1 right-1 p-0.5 bg-kortty-surface/80 rounded opacity-0 group-hover:opacity-100 transition-opacity text-kortty-text-dim hover:text-kortty-error"
              onClick={() => {
                if (node.id === "primary") {
                  onClosePrimarySplit?.();
                  return;
                }
                removeSplit(node.id);
              }}
            >
              <X className="w-3 h-3" />
            </button>
          )}
          {(ctrlShiftHeld || swapDrag) && allLeaves.length > 1 && (
            <div
              className={`absolute inset-0 z-20 flex items-center justify-center transition-colors ${
                swapDrag?.sourceId === node.id
                  ? "bg-kortty-accent/20 border-2 border-kortty-accent cursor-grabbing"
                  : swapDrag?.targetId === node.id
                    ? "bg-kortty-success/15 border-2 border-dashed border-kortty-success"
                    : swapDrag
                      ? "bg-transparent"
                      : "bg-kortty-accent/5 cursor-grab"
              }`}
              onMouseDown={(e) => {
                if (!swapDrag) {
                  e.preventDefault();
                  e.stopPropagation();
                  setSwapDrag({ sourceId: node.id, targetId: null });
                }
              }}
              onMouseEnter={() => {
                if (swapDrag && node.id !== swapDrag.sourceId) {
                  setSwapDrag((prev) => prev ? { ...prev, targetId: node.id } : null);
                }
              }}
              onMouseLeave={() => {
                if (swapDrag?.targetId === node.id) {
                  setSwapDrag((prev) => prev ? { ...prev, targetId: null } : null);
                }
              }}
            >
              {swapDrag?.sourceId === node.id && (
                <span className="text-xs text-kortty-accent bg-kortty-bg/80 px-2 py-1 rounded font-medium pointer-events-none">
                  Moving…
                </span>
              )}
              {swapDrag?.targetId === node.id && (
                <span className="text-xs text-kortty-success bg-kortty-bg/80 px-2 py-1 rounded font-medium pointer-events-none">
                  Drop here
                </span>
              )}
              {!swapDrag && (
                <GripVertical className="w-6 h-6 text-kortty-accent/40 pointer-events-none" />
              )}
            </div>
          )}
        </div>
      );
    }

    return (
      <PanelGroup
        direction={node.direction}
        ref={(handle: ImperativePanelGroupHandle | null) => {
          if (handle) panelGroupRefs.current.set(node.id, handle);
          else panelGroupRefs.current.delete(node.id);
        }}
      >
        {node.children.map((child, idx) => (
          <Fragment key={child.id}>
            {idx > 0 && (
              <PanelResizeHandle
                hitAreaMargins={{ coarse: 6, fine: 2 }}
                className={
                  node.direction === "horizontal"
                    ? "w-1 bg-kortty-border hover:bg-kortty-accent transition-colors cursor-col-resize"
                    : "h-1 bg-kortty-border hover:bg-kortty-accent transition-colors cursor-row-resize"
                }
              />
            )}
            <Panel minSize={10} defaultSize={100 / node.children.length}>
              {renderNode(child)}
            </Panel>
          </Fragment>
        ))}
      </PanelGroup>
    );
  }

  return (
    <div className="relative flex flex-col w-full h-full">
      {allLeaves.length > 1 && (
        <div className="flex items-center gap-1 px-2 py-0.5 bg-kortty-surface border-b border-kortty-border">
          <button
            className={`p-1 rounded text-xs transition-colors ${
              broadcast
                ? "text-kortty-warning bg-kortty-warning/10"
                : "text-kortty-text-dim hover:text-kortty-text"
            }`}
            onClick={() => setBroadcast(!broadcast)}
            title="Broadcast input to all terminals"
          >
            <Radio className="w-3.5 h-3.5" />
          </button>
          {broadcast && (
            <span className="text-[10px] text-kortty-warning ml-1">BROADCAST</span>
          )}
        </div>
      )}
      <div className="flex-1 min-h-0 overflow-hidden">{renderNode(tree)}</div>

      {activeSessionIds.map((sessionId) => {
        const leaf = leafBySessionId.get(sessionId);
        if (!leaf) return null;
        const host = hostElementsRef.current.get(sessionId);
        if (!host) return null;
        const paneFontSize = getFontSizeForSession?.(sessionId) ?? fontSize;
        return (
          <TerminalPortal
            key={sessionId}
            host={host}
            sessionId={sessionId}
            connected={leaf.connected}
            readOnly={readOnly}
            promptHookEnabled={promptHookEnabled}
            showTimestamps={showTimestamps}
            fontSize={paneFontSize}
            theme={theme}
            fontFamily={fontFamily}
            broadcastTargets={broadcast ? broadcastTargetsBySessionId[sessionId] : undefined}
            onContextMenu={(e, selectedText) => openContextMenu(e, leaf.id, sessionId, selectedText)}
            onCloseRequest={
              leaf.id === "primary" && allLeaves.length <= 1
                ? onCloseRequest
                : leaf.id === "primary"
                  ? onClosePrimarySplit
                : leaf.id !== "primary"
                  ? () => removeSplit(leaf.id)
                  : undefined
            }
          />
        );
      })}

      {contextMenu && (
        <div
          ref={menuRef}
          className="fixed z-[100] bg-kortty-panel border border-kortty-border rounded-lg shadow-2xl py-1 min-w-[200px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <CtxItem label="Copy" shortcut="Ctrl+C" onClick={() => menuAction(() => triggerTerminalAction("copy"))} />
          <CtxItem label="Paste" shortcut="Ctrl+V" onClick={() => menuAction(() => triggerTerminalAction("paste"))} />
          {onAiAction && (
            <>
              <CtxSep />
              <CtxSubMenu label="AI">
                <CtxItem
                  label="Summarize"
                  disabled={!contextMenu.selectedText.trim()}
                  onClick={() =>
                    menuAction(() => onAiAction(contextMenu.sessionId, "Summarize", contextMenu.selectedText))
                  }
                />
                <CtxItem
                  label="Solve Problem"
                  disabled={!contextMenu.selectedText.trim()}
                  onClick={() =>
                    menuAction(() => onAiAction(contextMenu.sessionId, "SolveProblem", contextMenu.selectedText))
                  }
                />
                <CtxItem
                  label="Ask..."
                  disabled={!contextMenu.selectedText.trim()}
                  onClick={() =>
                    menuAction(() => onAiAction(contextMenu.sessionId, "Ask", contextMenu.selectedText))
                  }
                />
              </CtxSubMenu>
            </>
          )}
          <CtxSep />
          <CtxSubMenu label="Split Horizontal">
            <CtxItem label="Same Server" onClick={() => handleSplit("horizontal", "same")} />
            <CtxItem label="New Server…" onClick={() => handleSplit("horizontal", "new")} />
          </CtxSubMenu>
          <CtxSubMenu label="Split Vertical">
            <CtxItem label="Same Server" onClick={() => handleSplit("vertical", "same")} />
            <CtxItem label="New Server…" onClick={() => handleSplit("vertical", "new")} />
          </CtxSubMenu>
          <CtxSep />
          <CtxItem
            label={broadcast ? "✓ Broadcast Input" : "  Broadcast Input"}
            onClick={() => menuAction(() => setBroadcast((b) => !b))}
          />
          <CtxSep />
          <CtxItem
            label="Zoom In"
            shortcut="Ctrl+="
            onClick={() => menuAction(() => currentContextLeaf && onZoomIn(currentContextLeaf.sessionId))}
          />
          <CtxItem
            label="Zoom Out"
            shortcut="Ctrl+−"
            onClick={() => menuAction(() => currentContextLeaf && onZoomOut(currentContextLeaf.sessionId))}
          />
          <CtxItem
            label="Reset Zoom"
            shortcut="Ctrl+0"
            onClick={() => menuAction(() => currentContextLeaf && onResetZoom(currentContextLeaf.sessionId))}
          />
          <CtxSep />
          <CtxItem
            label={showTimestamps ? "✓ Command Timestamps" : "  Command Timestamps"}
            onClick={() => menuAction(onToggleTimestamps)}
          />
          <CtxSep />
          <CtxItem
            label="Reconnect"
            onClick={() =>
              menuAction(() => {
                if (currentContextLeaf) {
                  onReconnect(currentContextLeaf.sessionId);
                }
              })
            }
          />
        </div>
      )}
    </div>
  );
}

function CtxItem({
  label,
  shortcut,
  disabled = false,
  onClick,
}: {
  label: string;
  shortcut?: string;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`w-full flex items-center justify-between px-3 py-1.5 text-xs transition-colors ${
        disabled
          ? "text-kortty-text-dim/60 cursor-not-allowed"
          : "text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent"
      }`}
      onClick={disabled ? undefined : onClick}
      disabled={disabled}
    >
      <span>{label}</span>
      {shortcut && <span className="text-kortty-text-dim ml-4 text-[10px]">{shortcut}</span>}
    </button>
  );
}

function CtxSubMenu({ label, children }: { label: string; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div
      className="relative"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <div className="w-full flex items-center justify-between px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-accent/10 hover:text-kortty-accent transition-colors cursor-default">
        <span>{label}</span>
        <ChevronRight className="w-3 h-3 text-kortty-text-dim" />
      </div>
      {open && (
        <div className="absolute left-full top-0 bg-kortty-panel border border-kortty-border rounded-lg shadow-2xl py-1 min-w-[160px] z-[101]">
          {children}
        </div>
      )}
    </div>
  );
}

function CtxSep() {
  return <div className="my-1 border-t border-kortty-border" />;
}

interface TerminalPortalProps {
  host: HTMLDivElement;
  sessionId: string;
  connected: boolean;
  readOnly?: boolean;
  promptHookEnabled?: boolean;
  showTimestamps: boolean;
  fontSize: number;
  theme?: TerminalTheme;
  fontFamily?: string;
  broadcastTargets?: string[];
  onContextMenu: (e: React.MouseEvent<HTMLDivElement>, selectedText: string) => void;
  onCloseRequest?: () => void;
}

function TerminalPortal({
  host,
  sessionId,
  connected,
  readOnly = false,
  promptHookEnabled = true,
  showTimestamps,
  fontSize,
  theme,
  fontFamily,
  broadcastTargets,
  onContextMenu,
  onCloseRequest,
}: TerminalPortalProps) {
  return createPortal(
    <TerminalTab
      sessionId={sessionId}
      connected={connected}
      readOnly={readOnly}
      promptHookEnabled={promptHookEnabled}
      showTimestamps={showTimestamps}
      fontSize={fontSize}
      theme={theme}
      fontFamily={fontFamily}
      onContextMenu={onContextMenu}
      onCloseRequest={onCloseRequest}
      broadcastTargets={broadcastTargets}
    />,
    host,
  );
}
