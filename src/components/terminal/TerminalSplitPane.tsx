import { useState, useCallback, useEffect, useLayoutEffect, useRef, useMemo, Fragment } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { Panel, PanelGroup, PanelResizeHandle, type ImperativePanelGroupHandle } from "react-resizable-panels";
import { TerminalTab } from "./TerminalTab";
import { Radio, X, ChevronRight, GripVertical, ChevronDown, ChevronUp, Download, Square, RotateCcw, Minus, Plus } from "lucide-react";
import type {
  AgentActivity,
  AiAction,
  TerminalAgentApproval,
  TerminalAgentPasswordRequest,
  TerminalAgentRunState,
} from "../../types/ai";
import type { TerminalAgentPanelDock } from "../../store/settingsStore";

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

const AGENT_SPINNER_FRAMES = ["|", "/", "-", "\\"];
const AGENT_PANEL_COLLAPSED_HEIGHT = 42;
const AGENT_PANEL_SIDE_TITLE_HEIGHT = 28;
const DEFAULT_AGENT_PANEL_HEIGHT = 260;
const DEFAULT_AGENT_PANEL_SIDE_WIDTH = 420;
const MIN_AGENT_PANEL_SIDE_WIDTH = 360;
const MAX_AGENT_PANEL_SIDE_WIDTH = 720;

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

function formatAgentElapsed(elapsedMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) {
    return [hours, minutes, seconds].map((value) => value.toString().padStart(2, "0")).join(":");
  }

  return [minutes, seconds].map((value) => value.toString().padStart(2, "0")).join(":");
}

function getAllLeaves(node: SplitNode): LeafNode[] {
  if (node.type === "leaf") return [node];
  return node.children.flatMap(getAllLeaves);
}

function clampNumber(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
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
  agentCommandName?: string;
  agentCommandNameCaseInsensitive?: boolean;
  readOnly?: boolean;
  promptHookEnabled?: boolean;
  agentPanelDock?: TerminalAgentPanelDock;
  initialAgentPanelHeight?: number;
  initialAgentPanelSideWidth?: number;
  initialAgentPanelFontSize?: number;
  getAgentPanelLabel?: (sessionId: string, splitIndex: number) => string;
  onAgentPanelLayoutChange?: (layout: {
    terminalAgentPanelDock?: TerminalAgentPanelDock;
    terminalAgentPanelHeight?: number;
    terminalAgentPanelSideWidth?: number;
    terminalAgentPanelFontSize?: number;
  }) => void;
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
  onStartAgent?: (sessionId: string) => void;
  onStartAgentPlan?: (sessionId: string) => void;
  onAgentCommand?: (sessionId: string, rawCommand: string) => void;
  onApproveAgent?: (approval: TerminalAgentApproval) => void;
  onApproveAgentAlways?: (approval: TerminalAgentApproval) => void;
  onSubmitAgentPassword?: (request: TerminalAgentPasswordRequest, password: string) => void;
  onStopAgent?: (runId: string) => void;
  agentRunStates?: Record<string, TerminalAgentRunState | undefined>;
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
  agentCommandName,
  agentCommandNameCaseInsensitive = false,
  readOnly = false,
  promptHookEnabled = true,
  agentPanelDock = "bottom",
  initialAgentPanelHeight,
  initialAgentPanelSideWidth,
  initialAgentPanelFontSize,
  getAgentPanelLabel,
  onAgentPanelLayoutChange,
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
  onStartAgent,
  onStartAgentPlan,
  onAgentCommand,
  onApproveAgent,
  onApproveAgentAlways,
  onSubmitAgentPassword,
  onStopAgent,
  agentRunStates,
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

  useEffect(() => {
    setAgentPanelSideWidth(
      clampNumber(initialAgentPanelSideWidth ?? DEFAULT_AGENT_PANEL_SIDE_WIDTH, MIN_AGENT_PANEL_SIDE_WIDTH, MAX_AGENT_PANEL_SIDE_WIDTH),
    );
  }, [initialAgentPanelSideWidth]);

  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [swapDrag, setSwapDrag] = useState<{ sourceId: string; targetId: string | null } | null>(null);
  const [ctrlShiftHeld, setCtrlShiftHeld] = useState(false);
  const [agentPasswords, setAgentPasswords] = useState<Record<string, string>>({});
  const [agentActivityTick, setAgentActivityTick] = useState(() => Date.now());
  const [agentActivitiesByRun, setAgentActivitiesByRun] = useState<Record<string, AgentActivity[]>>({});
  const [agentRunHistoryBySession, setAgentRunHistoryBySession] = useState<Record<string, string[]>>({});
  const [selectedAgentRunBySession, setSelectedAgentRunBySession] = useState<Record<string, string>>({});
  const [closedAgentPanelBySession, setClosedAgentPanelBySession] = useState<Record<string, boolean>>({});
  const [collapsedAgentPanelBySession, setCollapsedAgentPanelBySession] = useState<Record<string, boolean>>({});
  const [agentPanelHeights, setAgentPanelHeights] = useState<Record<string, number>>({});
  const [agentPanelFontSizes, setAgentPanelFontSizes] = useState<Record<string, number>>({});
  const [agentPanelSideWidth, setAgentPanelSideWidth] = useState(() =>
    clampNumber(initialAgentPanelSideWidth ?? DEFAULT_AGENT_PANEL_SIDE_WIDTH, MIN_AGENT_PANEL_SIDE_WIDTH, MAX_AGENT_PANEL_SIDE_WIDTH),
  );
  const rootRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const panelGroupRefs = useRef<Map<string, ImperativePanelGroupHandle>>(new Map());
  const hostElementsRef = useRef<Map<string, HTMLDivElement>>(new Map());
  const slotRefs = useRef<Map<string, HTMLDivElement | null>>(new Map());
  const agentRunStartedAtRef = useRef<Record<string, number>>({});
  const agentRunSessionRef = useRef<Record<string, string>>({});
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

  const leafIndexBySessionId = useMemo(() => {
    const map = new Map<string, number>();
    allLeaves.forEach((leaf, index) => {
      map.set(leaf.sessionId, index);
    });
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

  function isActiveAgentState(state?: TerminalAgentRunState) {
    return state != null && !["Done", "Blocked", "Cancelled", "Failed"].includes(state.phase);
  }

  useEffect(() => {
    const now = Date.now();
    const activeRunIds = new Set<string>();
    const knownRunIds = new Set<string>();

    for (const state of Object.values(agentRunStates ?? {})) {
      if (!state?.runId) {
        continue;
      }
      knownRunIds.add(state.runId);
      agentRunSessionRef.current[state.runId] = state.sessionId;
      setAgentRunHistoryBySession((prev) => {
        const existing = prev[state.sessionId] ?? [];
        if (existing.includes(state.runId)) {
          return prev;
        }
        return {
          ...prev,
          [state.sessionId]: [...existing, state.runId],
        };
      });
      setSelectedAgentRunBySession((prev) => ({
        ...prev,
        [state.sessionId]: prev[state.sessionId] ?? state.runId,
      }));
      if (!isActiveAgentState(state)) {
        continue;
      }
      activeRunIds.add(state.runId);
      if (agentRunStartedAtRef.current[state.runId] == null) {
        agentRunStartedAtRef.current[state.runId] = now;
      }
      setClosedAgentPanelBySession((prev) => ({ ...prev, [state.sessionId]: false }));
      setSelectedAgentRunBySession((prev) => ({ ...prev, [state.sessionId]: state.runId }));
    }

    for (const runId of Object.keys(agentRunStartedAtRef.current)) {
      if (!knownRunIds.has(runId)) {
        delete agentRunStartedAtRef.current[runId];
      }
    }

    if (activeRunIds.size > 0 || knownRunIds.size > 0) {
      setAgentActivityTick(now);
    }
  }, [agentRunStates]);

  useEffect(() => {
    let disposed = false;
    const unlisten = listen<AgentActivity>("terminal-agent-activity", (event) => {
      if (disposed) return;
      const runId = event.payload.id.split(":")[0];
      if (!runId) return;
      setAgentActivitiesByRun((prev) => {
        const existing = prev[runId] ?? [];
        const index = existing.findIndex((activity) => activity.id === event.payload.id);
        const nextActivities =
          index >= 0
            ? existing.map((activity, i) => (i === index ? event.payload : activity))
            : [...existing, event.payload];
        return {
          ...prev,
          [runId]: nextActivities,
        };
      });
      const sessionId = agentRunSessionRef.current[runId];
      if (sessionId) {
        setClosedAgentPanelBySession((prev) => ({ ...prev, [sessionId]: false }));
      }
    });
    return () => {
      disposed = true;
      unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    function handleAgentCancel(event: Event) {
      const custom = event as CustomEvent<{ sessionId: string }>;
      const sessionId = custom.detail?.sessionId;
      const runId = sessionId ? agentRunStates?.[sessionId]?.runId : undefined;
      if (runId && isActiveAgentState(agentRunStates?.[sessionId])) {
        onStopAgent?.(runId);
      }
    }
    window.addEventListener("kortty-terminal-agent-cancel", handleAgentCancel as EventListener);
    return () => {
      window.removeEventListener("kortty-terminal-agent-cancel", handleAgentCancel as EventListener);
    };
  }, [agentRunStates, onStopAgent]);

  useEffect(() => {
    const hasActiveAgent = Object.values(agentRunStates ?? {}).some((state) => isActiveAgentState(state));
    if (!hasActiveAgent) {
      return;
    }

    const timer = window.setInterval(() => {
      setAgentActivityTick(Date.now());
    }, 200);

    return () => {
      window.clearInterval(timer);
    };
  }, [agentRunStates]);

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

  const handleAgentPanelDockDragEnd = useCallback((clientX: number, clientY: number) => {
    const rect = rootRef.current?.getBoundingClientRect();
    if (!rect) {
      return;
    }

    const candidates: { dock: TerminalAgentPanelDock; distance: number }[] = [
      { dock: "left", distance: Math.abs(clientX - rect.left) },
      { dock: "right", distance: Math.abs(rect.right - clientX) },
      { dock: "bottom", distance: Math.abs(rect.bottom - clientY) },
    ];
    candidates.sort((a, b) => a.distance - b.distance);
    const nextDock = candidates[0]?.dock ?? "bottom";
    if (nextDock !== agentPanelDock) {
      onAgentPanelLayoutChange?.({ terminalAgentPanelDock: nextDock });
    }
  }, [agentPanelDock, onAgentPanelLayoutChange]);

  const startAgentPanelSideResize = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = agentPanelSideWidth;
    const direction = agentPanelDock === "left" ? 1 : -1;

    function onMove(moveEvent: MouseEvent) {
      const nextWidth = clampNumber(
        startWidth + (moveEvent.clientX - startX) * direction,
        MIN_AGENT_PANEL_SIDE_WIDTH,
        MAX_AGENT_PANEL_SIDE_WIDTH,
      );
      setAgentPanelSideWidth(nextWidth);
      onAgentPanelLayoutChange?.({ terminalAgentPanelSideWidth: nextWidth });
    }

    function onUp() {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    }

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }, [agentPanelDock, agentPanelSideWidth, onAgentPanelLayoutChange]);

  function getAgentPanelContext(node: LeafNode) {
    const agentState = agentRunStates?.[node.sessionId];
    if (agentState == null || closedAgentPanelBySession[node.sessionId]) {
      return null;
    }

    const pendingApproval = agentState.pendingApproval;
    const pendingPasswordRequest = agentState.pendingPasswordRequest;
    const activeAgentState = isActiveAgentState(agentState);
    const passwordValue = agentPasswords[node.sessionId] ?? "";
    const startedAt =
      agentState.runId != null ? agentRunStartedAtRef.current[agentState.runId] : undefined;
    const elapsedMs = startedAt != null ? Math.max(0, agentActivityTick - startedAt) : 0;
    const spinnerFrame = startedAt != null
      ? AGENT_SPINNER_FRAMES[Math.floor(elapsedMs / 200) % AGENT_SPINNER_FRAMES.length]
      : AGENT_SPINNER_FRAMES[0];
    const agentPanelCollapsed = !!collapsedAgentPanelBySession[node.sessionId];
    const agentPanelExpandedHeight =
      agentPanelHeights[node.sessionId] ??
      initialAgentPanelHeight ??
      DEFAULT_AGENT_PANEL_HEIGHT;
    const selectedRunId = selectedAgentRunBySession[node.sessionId] || agentState.runId;
    const activities = agentActivitiesByRun[selectedRunId] ?? [];
    const runIds = agentRunHistoryBySession[node.sessionId] ?? [agentState.runId];
    const splitIndex = leafIndexBySessionId.get(node.sessionId) ?? 0;
    const connectionLabel =
      getAgentPanelLabel?.(node.sessionId, splitIndex) ??
      `Session ${node.sessionId.slice(0, 8)} · ${splitIndex === 0 ? "Main" : `Split ${splitIndex + 1}`}`;

    return {
      agentState,
      activeAgentState,
      activities,
      agentPanelCollapsed,
      agentPanelExpandedHeight,
      connectionLabel,
      elapsedMs,
      passwordValue,
      pendingApproval,
      pendingPasswordRequest,
      runId: selectedRunId,
      runIds,
      selectedRunId,
      spinnerFrame,
    };
  }

  function renderAgentPanel(node: LeafNode, dock: TerminalAgentPanelDock) {
    const context = getAgentPanelContext(node);
    if (!context) {
      return null;
    }

    return (
      <AgentActivityPanel
        key={`${node.sessionId}:${dock}`}
        sessionId={node.sessionId}
        dock={dock}
        connectionLabel={dock === "bottom" ? undefined : context.connectionLabel}
        agentState={context.agentState}
        active={context.activeAgentState}
        spinnerFrame={context.spinnerFrame}
        elapsedMs={context.elapsedMs}
        activities={context.activities}
        runIds={context.runIds}
        selectedRunId={context.selectedRunId}
        collapsed={context.agentPanelCollapsed}
        height={context.agentPanelExpandedHeight}
        fontSize={
          agentPanelFontSizes[node.sessionId] ??
          initialAgentPanelFontSize ??
          12
        }
        pendingApproval={context.pendingApproval}
        pendingPasswordRequest={context.pendingPasswordRequest}
        passwordValue={context.passwordValue}
        onPasswordChange={(value) =>
          setAgentPasswords((prev) => ({ ...prev, [node.sessionId]: value }))
        }
        onSubmitPassword={() => {
          if (!context.pendingPasswordRequest || !context.passwordValue.trim()) {
            return;
          }
          onSubmitAgentPassword?.(context.pendingPasswordRequest, context.passwordValue);
          setAgentPasswords((prev) => ({ ...prev, [node.sessionId]: "" }));
        }}
        onApprove={() => context.pendingApproval && onApproveAgent?.(context.pendingApproval)}
        onApproveAlways={() => context.pendingApproval && onApproveAgentAlways?.(context.pendingApproval)}
        onCancel={() => context.agentState.runId && onStopAgent?.(context.agentState.runId)}
        onClose={() =>
          setClosedAgentPanelBySession((prev) => ({ ...prev, [node.sessionId]: true }))
        }
        onToggleCollapsed={() =>
          setCollapsedAgentPanelBySession((prev) => ({
            ...prev,
            [node.sessionId]: !prev[node.sessionId],
          }))
        }
        onSelectRun={(runId) =>
          setSelectedAgentRunBySession((prev) => ({ ...prev, [node.sessionId]: runId }))
        }
        onRerun={() => {
          const prompt = extractAgentPrompt(context.agentState, agentActivitiesByRun[context.runId] ?? []);
          onAgentCommand?.(node.sessionId, `${agentCommandName ?? "agent"} ${prompt}`);
        }}
        onHeightChange={(height) => {
          const nextHeight = clampNumber(height, 140, 520);
          setAgentPanelHeights((prev) => ({ ...prev, [node.sessionId]: nextHeight }));
          onAgentPanelLayoutChange?.({ terminalAgentPanelHeight: nextHeight });
        }}
        onFontSizeChange={(size) => {
          const nextFontSize = clampNumber(size, 9, 20);
          setAgentPanelFontSizes((prev) => ({ ...prev, [node.sessionId]: nextFontSize }));
          onAgentPanelLayoutChange?.({ terminalAgentPanelFontSize: nextFontSize });
        }}
        onDockDragEnd={handleAgentPanelDockDragEnd}
        buildAllRuns={() => {
          const runIds = agentRunHistoryBySession[node.sessionId] ?? [];
          return runIds.map((runId) => ({
            runId,
            activities: agentActivitiesByRun[runId] ?? [],
          }));
        }}
      />
    );
  }

  function renderAgentSideRail() {
    const panels = allLeaves
      .map((leaf) => renderAgentPanel(leaf, agentPanelDock))
      .filter((panel): panel is NonNullable<typeof panel> => panel != null);

    if (panels.length === 0 || agentPanelDock === "bottom") {
      return null;
    }

    const resizeHandle = (
      <div
        className="w-1 shrink-0 cursor-col-resize bg-kortty-border/70 hover:bg-kortty-accent"
        onMouseDown={startAgentPanelSideResize}
      />
    );

    return (
      <div
        className={`flex h-full min-h-0 shrink-0 bg-kortty-surface/95 ${
          agentPanelDock === "left" ? "border-r border-kortty-border" : "border-l border-kortty-border"
        }`}
        style={{ width: agentPanelSideWidth }}
      >
        {agentPanelDock === "right" && resizeHandle}
        <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-1">
          {panels}
        </div>
        {agentPanelDock === "left" && resizeHandle}
      </div>
    );
  }

  function renderNode(node: SplitNode): React.ReactNode {
    if (node.type === "leaf") {
      const agentPanelContext = getAgentPanelContext(node);
      const reservedAgentPanelHeight = agentPanelDock === "bottom" && agentPanelContext
        ? agentPanelContext.agentPanelCollapsed
          ? AGENT_PANEL_COLLAPSED_HEIGHT
          : agentPanelContext.agentPanelExpandedHeight
        : 0;
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
            style={{ bottom: reservedAgentPanelHeight }}
          />
          {agentPanelDock === "bottom" && renderAgentPanel(node, "bottom")}
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
    <div ref={rootRef} className="relative flex flex-col w-full h-full">
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
      <div className="flex flex-1 min-h-0 overflow-hidden">
        {agentPanelDock === "left" && renderAgentSideRail()}
        <div className="flex-1 min-w-0 min-h-0 overflow-hidden">{renderNode(tree)}</div>
        {agentPanelDock === "right" && renderAgentSideRail()}
      </div>

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
            agentCommandName={agentCommandName}
            agentCommandNameCaseInsensitive={agentCommandNameCaseInsensitive}
            readOnly={readOnly || isActiveAgentState(agentRunStates?.[sessionId])}
            forceAutoScroll={isActiveAgentState(agentRunStates?.[sessionId])}
            promptHookEnabled={promptHookEnabled}
            showTimestamps={showTimestamps}
            fontSize={paneFontSize}
            theme={theme}
            fontFamily={fontFamily}
            broadcastTargets={broadcast ? broadcastTargetsBySessionId[sessionId] : undefined}
            onContextMenu={(e, selectedText) => openContextMenu(e, leaf.id, sessionId, selectedText)}
            onAgentCommand={onAgentCommand}
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
              <CtxItem
                label="Agent..."
                onClick={() => menuAction(() => onStartAgent?.(contextMenu.sessionId))}
              />
              <CtxItem
                label="Planning..."
                onClick={() => menuAction(() => onStartAgentPlan?.(contextMenu.sessionId))}
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

type AgentExportFormat = "markdown" | "text" | "yaml" | "xml" | "json" | "pdf" | "asciidoc";

type AgentExportRun = {
  runId: string;
  activities: AgentActivity[];
};

interface AgentActivityPanelProps {
  sessionId: string;
  dock: TerminalAgentPanelDock;
  connectionLabel?: string;
  agentState: TerminalAgentRunState;
  active: boolean;
  spinnerFrame: string;
  elapsedMs: number;
  activities: AgentActivity[];
  runIds: string[];
  selectedRunId: string;
  collapsed: boolean;
  height: number;
  fontSize: number;
  pendingApproval?: TerminalAgentApproval;
  pendingPasswordRequest?: TerminalAgentPasswordRequest;
  passwordValue: string;
  onPasswordChange: (value: string) => void;
  onSubmitPassword: () => void;
  onApprove: () => void;
  onApproveAlways: () => void;
  onCancel: () => void;
  onClose: () => void;
  onToggleCollapsed: () => void;
  onSelectRun: (runId: string) => void;
  onRerun: () => void;
  onHeightChange: (height: number) => void;
  onFontSizeChange: (fontSize: number) => void;
  onDockDragEnd: (clientX: number, clientY: number) => void;
  buildAllRuns: () => AgentExportRun[];
}

function AgentActivityPanel({
  sessionId,
  dock,
  connectionLabel,
  agentState,
  active,
  spinnerFrame,
  elapsedMs,
  activities,
  runIds,
  selectedRunId,
  collapsed,
  height,
  fontSize,
  pendingApproval,
  pendingPasswordRequest,
  passwordValue,
  onPasswordChange,
  onSubmitPassword,
  onApprove,
  onApproveAlways,
  onCancel,
  onClose,
  onToggleCollapsed,
  onSelectRun,
  onRerun,
  onHeightChange,
  onFontSizeChange,
  onDockDragEnd,
  buildAllRuns,
}: AgentActivityPanelProps) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  const [collapsedIds, setCollapsedIds] = useState<Set<string>>(new Set());
  const [exportFormat, setExportFormat] = useState<AgentExportFormat>("markdown");
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const activityViewportRef = useRef<HTMLDivElement | null>(null);
  const selectedIndex = Math.max(0, runIds.indexOf(selectedRunId));
  const tokenUsage = activities.reduce(
    (sum, activity) =>
      activity.tokenUsage?.known ? sum + activity.tokenUsage.totalTokens : sum,
    0,
  );
  const activityScrollKey = activities
    .map((activity) => `${activity.id}:${activity.status}:${activity.summary.length}:${activity.detail.length}`)
    .join("|");

  useLayoutEffect(() => {
    if (collapsed) {
      return;
    }
    const viewport = activityViewportRef.current;
    if (!viewport) {
      return;
    }
    viewport.scrollTo({ top: viewport.scrollHeight, behavior: active ? "smooth" : "auto" });
  }, [activityScrollKey, active, collapsed, selectedRunId]);

  function startResize(event: React.MouseEvent<HTMLDivElement>) {
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = height;
    function onMove(moveEvent: MouseEvent) {
      onHeightChange(Math.min(520, Math.max(140, startHeight + startY - moveEvent.clientY)));
    }
    function onUp() {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  function startDockDrag(event: React.MouseEvent<HTMLButtonElement>) {
    event.preventDefault();
    event.stopPropagation();
    const startX = event.clientX;
    const startY = event.clientY;
    let hasMoved = false;
    const previousCursor = document.body.style.cursor;
    document.body.style.cursor = "grabbing";

    function onMove(moveEvent: MouseEvent) {
      if (Math.abs(moveEvent.clientX - startX) > 8 || Math.abs(moveEvent.clientY - startY) > 8) {
        hasMoved = true;
      }
    }

    function onUp(upEvent: MouseEvent) {
      document.body.style.cursor = previousCursor;
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      if (hasMoved) {
        onDockDragEnd(upEvent.clientX, upEvent.clientY);
      }
    }

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  function isActivityExpanded(activity: AgentActivity) {
    if (!activity.collapsible) {
      return false;
    }
    if (collapsedIds.has(activity.id)) {
      return false;
    }
    if (expandedIds.has(activity.id)) {
      return true;
    }
    return !activity.collapsed;
  }

  function toggleActivity(activity: AgentActivity) {
    if (!activity.collapsible) {
      return;
    }
    const expanded = isActivityExpanded(activity);
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (expanded) {
        next.delete(activity.id);
      } else {
        next.add(activity.id);
      }
      return next;
    });
    setCollapsedIds((prev) => {
      const next = new Set(prev);
      if (expanded) {
        next.add(activity.id);
      } else {
        next.delete(activity.id);
      }
      return next;
    });
  }

  function toggleAllActivities() {
    const expandableIds = activities
      .filter((activity) => activity.collapsible)
      .map((activity) => activity.id);
    if (expandableIds.length === 0) {
      return;
    }

    if (expandableIds.every((id) => {
      const activity = activities.find((candidate) => candidate.id === id);
      return activity != null && isActivityExpanded(activity);
    })) {
      setExpandedIds((prev) => {
        const next = new Set(prev);
        for (const id of expandableIds) {
          next.delete(id);
        }
        return next;
      });
      setCollapsedIds((prev) => {
        const next = new Set(prev);
        for (const id of expandableIds) {
          next.add(id);
        }
        return next;
      });
      return;
    }

    setExpandedIds((prev) => {
      const next = new Set(prev);
      for (const id of expandableIds) {
        next.add(id);
      }
      return next;
    });
    setCollapsedIds((prev) => {
      const next = new Set(prev);
      for (const id of expandableIds) {
        next.delete(id);
      }
      return next;
    });
  }

  async function exportRuns(scope: "current" | "all") {
    setExportStatus(null);
    try {
      const runs =
        scope === "current"
          ? [{ runId: selectedRunId, activities }]
          : buildAllRuns().filter((run) => run.activities.length > 0);
      if (runs.length === 0) {
        return;
      }
      const extension = exportExtension(exportFormat);
      const target = await saveDialog({
        defaultPath: `kortty-agent-${scope}.${extension}`,
        filters: [{ name: exportFormat.toUpperCase(), extensions: [extension] }],
      });
      if (typeof target === "string" && target.trim()) {
        if (exportFormat === "pdf") {
          await invoke("export_terminal_agent_activity_pdf", {
            path: target,
            text: buildTextExport(runs),
          });
        } else {
          await writeTextFile(target, buildAgentExportContent(exportFormat, runs));
        }
        setExportStatus(`Exported to ${target}`);
      }
    } catch (error) {
      console.error("Failed to export terminal agent activity:", error);
      setExportStatus(`Export failed: ${String(error)}`);
    }
  }

  const hasSideTitle = dock !== "bottom" && !!connectionLabel;
  const sideTitleHeight = hasSideTitle ? AGENT_PANEL_SIDE_TITLE_HEIGHT : 0;
  const visibleHeight = collapsed
    ? AGENT_PANEL_COLLAPSED_HEIGHT + sideTitleHeight
    : height;
  const contentChromeHeight = AGENT_PANEL_COLLAPSED_HEIGHT + sideTitleHeight;
  const panelClassName = dock === "bottom"
    ? "absolute left-0 right-0 bottom-0 z-30 border-t border-kortty-border bg-kortty-surface/95 shadow-2xl"
    : "relative w-full shrink-0 overflow-hidden rounded border border-kortty-border bg-kortty-surface/95 shadow-xl";
  const sideLayout = dock !== "bottom";
  const expandableActivityCount = activities.filter((activity) => activity.collapsible).length;
  const allExpandableExpanded =
    expandableActivityCount > 0 &&
    activities
      .filter((activity) => activity.collapsible)
      .every((activity) => isActivityExpanded(activity));
  const controlsPanel = (
    <div
      className={
        sideLayout
          ? "shrink-0 border-b border-kortty-border p-2 text-[11px] text-kortty-text-dim"
          : "w-64 shrink-0 border-l border-kortty-border p-2 text-[11px] text-kortty-text-dim space-y-2"
      }
    >
      <div className={sideLayout ? "flex flex-wrap items-center gap-2" : "space-y-2"}>
        {pendingPasswordRequest && (
          <div className={sideLayout ? "flex min-w-[220px] items-center gap-1" : "space-y-1"}>
            <input
              type="password"
              className="input-field text-xs"
              placeholder="sudo password"
              value={passwordValue}
              onChange={(event) => onPasswordChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && passwordValue.trim()) {
                  onSubmitPassword();
                }
              }}
            />
            <button className={`btn-primary text-xs ${sideLayout ? "shrink-0" : "w-full"}`} disabled={!passwordValue.trim()} onClick={onSubmitPassword}>
              Unlock
            </button>
          </div>
        )}
        {pendingApproval && (
          <div className="flex gap-1">
            <button className="btn-primary flex-1 text-xs" onClick={onApprove}>Approve</button>
            <button className="btn-secondary flex-1 text-xs" onClick={onApproveAlways}>Allow always</button>
          </div>
        )}
        <div className={sideLayout ? "flex min-w-[96px] items-center gap-1" : "flex items-center gap-1"}>
          <button className="icon-button" title="Decrease font" onClick={() => onFontSizeChange(Math.max(9, fontSize - 1))}>
            <Minus className="w-3.5 h-3.5" />
          </button>
          <span className="flex-1 text-center font-mono">{fontSize}px</span>
          <button className="icon-button" title="Increase font" onClick={() => onFontSizeChange(Math.min(20, fontSize + 1))}>
            <Plus className="w-3.5 h-3.5" />
          </button>
        </div>
        <button
          className={`btn-secondary text-xs ${sideLayout ? "shrink-0" : "w-full"}`}
          disabled={expandableActivityCount === 0}
          onClick={toggleAllActivities}
        >
          {allExpandableExpanded ? "Collapse all" : "Expand all"}
        </button>
        <select className={`input-field text-xs ${sideLayout ? "w-36 shrink-0" : ""}`} value={exportFormat} onChange={(event) => setExportFormat(event.target.value as AgentExportFormat)}>
          <option value="markdown">Markdown</option>
          <option value="text">Plain text</option>
          <option value="yaml">YAML</option>
          <option value="xml">XML</option>
          <option value="json">JSON</option>
          <option value="pdf">PDF</option>
          <option value="asciidoc">Asciidoctor</option>
        </select>
        <button className={`btn-secondary flex items-center justify-center gap-2 text-xs ${sideLayout ? "shrink-0" : "w-full"}`} onClick={() => void exportRuns("current")}>
          <Download className="w-3.5 h-3.5" /> Current run
        </button>
        <button className={`btn-secondary flex items-center justify-center gap-2 text-xs ${sideLayout ? "shrink-0" : "w-full"}`} onClick={() => void exportRuns("all")}>
          <Download className="w-3.5 h-3.5" /> All runs
        </button>
        {exportStatus && (
          <div className={`rounded border border-kortty-border bg-kortty-bg/60 px-2 py-1 text-[10px] text-kortty-text-dim ${sideLayout ? "min-w-0 flex-1 truncate" : ""}`}>
            {exportStatus}
          </div>
        )}
        <div className={`font-mono text-[10px] text-kortty-text-dim truncate ${sideLayout ? "min-w-[120px] max-w-full" : ""}`}>{sessionId}</div>
      </div>
    </div>
  );

  return (
    <div
      className={panelClassName}
      style={{ height: visibleHeight }}
    >
      {hasSideTitle && (
        <div className="flex h-7 min-w-0 items-center gap-2 border-b border-kortty-border bg-kortty-bg/60 px-2 text-[11px]">
          <span className="truncate font-medium text-kortty-text">{connectionLabel}</span>
          <span className="ml-auto shrink-0 text-kortty-text-dim">AI Agent</span>
        </div>
      )}
      <div className="h-1 cursor-row-resize bg-kortty-border/60 hover:bg-kortty-accent" onMouseDown={startResize} />
      <div className="flex h-9 min-w-0 items-center gap-2 overflow-hidden px-2 text-[11px] border-b border-kortty-border">
        <button className="icon-button cursor-grab active:cursor-grabbing" title="Move AI Agent panel" onMouseDown={startDockDrag}>
          <GripVertical className="w-3.5 h-3.5" />
        </button>
        <span className="font-medium text-kortty-accent whitespace-nowrap">AI Agent</span>
        {active && <span className="font-mono text-kortty-accent">{spinnerFrame}</span>}
        <span className="font-mono text-kortty-text-dim">{formatAgentElapsed(elapsedMs)}</span>
        <span className="text-kortty-text-dim whitespace-nowrap">{agentState.phase}</span>
        <span className="min-w-0 flex-1 truncate text-kortty-text">{agentState.userMessage || agentState.summary}</span>
        <span className="shrink-0 whitespace-nowrap text-kortty-text-dim">
          {tokenUsage > 0 ? `${tokenUsage} tokens` : "tokens unknown"}
        </span>
        <button className="icon-button" title="Previous run" disabled={selectedIndex <= 0} onClick={() => onSelectRun(runIds[selectedIndex - 1])}>
          <ChevronUp className="w-3.5 h-3.5" />
        </button>
        <button className="icon-button" title="Next run" disabled={selectedIndex >= runIds.length - 1} onClick={() => onSelectRun(runIds[selectedIndex + 1])}>
          <ChevronDown className="w-3.5 h-3.5" />
        </button>
        <button className="icon-button" title="Rerun" onClick={onRerun}>
          <RotateCcw className="w-3.5 h-3.5" />
        </button>
        {active && (
          <button className="icon-button text-kortty-error" title="Cancel" onClick={onCancel}>
            <Square className="w-3.5 h-3.5" />
          </button>
        )}
        <button className="icon-button" title={collapsed ? "Expand panel" : "Collapse panel"} onClick={onToggleCollapsed}>
          {collapsed ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
        </button>
        <button className="icon-button" title="Close panel" onClick={onClose}>
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
      {!collapsed && (
        <div className={sideLayout ? "flex min-h-0 flex-col" : "flex min-h-0"} style={{ height: `calc(100% - ${contentChromeHeight}px)` }}>
          {sideLayout && controlsPanel}
          <div
            ref={activityViewportRef}
            className="flex-1 min-w-0 overflow-y-auto px-2 py-2"
            style={{ fontSize }}
          >
            {activities.length === 0 ? (
              <div className="text-kortty-text-dim">Waiting for agent activity.</div>
            ) : (
              activities.map((activity) => {
                const expanded = isActivityExpanded(activity);
                return (
                  <div key={activity.id} className="mb-1 rounded border border-kortty-border bg-kortty-bg/70">
                    <button
                      className="flex w-full items-start gap-2 px-2 py-1.5 text-left"
                      onClick={() => activity.collapsible && toggleActivity(activity)}
                    >
                      <span className={`mt-1 h-2 w-2 rounded-full ${activityDotClass(activity)}`} />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-kortty-text">{activityTitle(activity)}</span>
                        <span className="block truncate text-kortty-text-dim">{activity.summary}</span>
                      </span>
                      <span className="font-mono text-kortty-text-dim whitespace-nowrap">
                        {activity.elapsedSeconds}s
                      </span>
                      <span className="font-mono text-kortty-text-dim whitespace-nowrap">
                        {formatTokenUsage(activity)}
                      </span>
                    </button>
                    {expanded && activity.detail.trim() && (
                      <pre className="max-h-48 overflow-auto whitespace-pre-wrap border-t border-kortty-border px-2 py-1.5 font-mono text-kortty-text-dim">
                        {activity.detail}
                      </pre>
                    )}
                  </div>
                );
              })
            )}
          </div>
          {!sideLayout && controlsPanel}
        </div>
      )}
    </div>
  );
}

function extractAgentPrompt(state: TerminalAgentRunState, activities: AgentActivity[]) {
  const startDetail = activities.find((activity) => activity.id.endsWith(":message:start"))?.detail;
  return (startDetail || state.userMessage || state.summary || "").trim();
}

function activityTitle(activity: AgentActivity) {
  return `${activity.activityType} · ${activity.status} · ${activity.title}`;
}

function activityDotClass(activity: AgentActivity) {
  if (activity.status === "Running") return "bg-kortty-accent animate-pulse";
  if (activity.status === "Completed") return "bg-kortty-success";
  if (activity.status === "Cancelled") return "bg-kortty-warning";
  return "bg-kortty-error";
}

function formatTokenUsage(activity: AgentActivity) {
  const usage = activity.tokenUsage;
  if (!usage?.known) return "-";
  return `${usage.totalTokens}`;
}

function exportExtension(format: AgentExportFormat) {
  switch (format) {
    case "markdown": return "md";
    case "text": return "txt";
    case "yaml": return "yaml";
    case "xml": return "xml";
    case "json": return "json";
    case "pdf": return "pdf";
    case "asciidoc": return "adoc";
  }
}

function buildAgentExportContent(format: AgentExportFormat, runs: AgentExportRun[]) {
  switch (format) {
    case "json":
      return JSON.stringify({ exportedAt: new Date().toISOString(), runs }, null, 2);
    case "yaml":
      return buildYamlExport(runs);
    case "xml":
      return buildXmlExport(runs);
    case "text":
      return buildTextExport(runs);
    case "asciidoc":
      return buildAsciiDocExport(runs);
    case "pdf":
      return buildTextExport(runs);
    case "markdown":
      return buildMarkdownExport(runs);
  }
}

function buildMarkdownExport(runs: AgentExportRun[]) {
  return runs.map((run) => [
    `# Terminal Agent Run ${run.runId}`,
    ...run.activities.map((activity) =>
      `## ${activityTitle(activity)}\n\n${activity.summary}\n\n${activity.detail ? "```text\n" + activity.detail + "\n```" : ""}`,
    ),
  ].join("\n\n")).join("\n\n");
}

function buildTextExport(runs: AgentExportRun[]) {
  return runs.map((run) => [
    `Terminal Agent Run ${run.runId}`,
    ...run.activities.map((activity) =>
      `${activityTitle(activity)}\n${activity.summary}${activity.detail ? "\n" + activity.detail : ""}`,
    ),
  ].join("\n\n")).join("\n\n");
}

function buildAsciiDocExport(runs: AgentExportRun[]) {
  return runs.map((run) => [
    `= Terminal Agent Run ${run.runId}`,
    ...run.activities.map((activity) =>
      `== ${activityTitle(activity)}\n\n${activity.summary}${activity.detail ? "\n\n----\n" + activity.detail + "\n----" : ""}`,
    ),
  ].join("\n\n")).join("\n\n");
}

function buildYamlExport(runs: AgentExportRun[]) {
  const lines = [`exportedAt: ${JSON.stringify(new Date().toISOString())}`, "runs:"];
  for (const run of runs) {
    lines.push(`  - runId: ${JSON.stringify(run.runId)}`, "    activities:");
    for (const activity of run.activities) {
      lines.push(
        `      - id: ${JSON.stringify(activity.id)}`,
        `        type: ${JSON.stringify(activity.activityType)}`,
        `        status: ${JSON.stringify(activity.status)}`,
        `        title: ${JSON.stringify(activity.title)}`,
        `        summary: ${JSON.stringify(activity.summary)}`,
        `        detail: ${JSON.stringify(activity.detail)}`,
        `        elapsedSeconds: ${activity.elapsedSeconds}`,
        `        totalTokens: ${activity.tokenUsage?.known ? activity.tokenUsage.totalTokens : 0}`,
      );
    }
  }
  return lines.join("\n");
}

function buildXmlExport(runs: AgentExportRun[]) {
  return [
    `<?xml version="1.0" encoding="UTF-8"?>`,
    `<terminalAgentExport exportedAt="${escapeXml(new Date().toISOString())}">`,
    ...runs.map((run) =>
      `  <run id="${escapeXml(run.runId)}">\n${run.activities.map((activity) =>
        `    <activity id="${escapeXml(activity.id)}" type="${escapeXml(activity.activityType)}" status="${escapeXml(activity.status)}" elapsedSeconds="${activity.elapsedSeconds}">\n      <title>${escapeXml(activity.title)}</title>\n      <summary>${escapeXml(activity.summary)}</summary>\n      <detail>${escapeXml(activity.detail)}</detail>\n    </activity>`,
      ).join("\n")}\n  </run>`,
    ),
    `</terminalAgentExport>`,
  ].join("\n");
}

function escapeXml(value: string) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
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
  agentCommandName?: string;
  agentCommandNameCaseInsensitive?: boolean;
  readOnly?: boolean;
  forceAutoScroll?: boolean;
  promptHookEnabled?: boolean;
  showTimestamps: boolean;
  fontSize: number;
  theme?: TerminalTheme;
  fontFamily?: string;
  broadcastTargets?: string[];
  onContextMenu: (e: React.MouseEvent<HTMLDivElement>, selectedText: string) => void;
  onAgentCommand?: (sessionId: string, rawCommand: string) => void;
  onCloseRequest?: () => void;
}

function TerminalPortal({
  host,
  sessionId,
  connected,
  agentCommandName,
  agentCommandNameCaseInsensitive = false,
  readOnly = false,
  forceAutoScroll = false,
  promptHookEnabled = true,
  showTimestamps,
  fontSize,
  theme,
  fontFamily,
  broadcastTargets,
  onContextMenu,
  onAgentCommand,
  onCloseRequest,
}: TerminalPortalProps) {
  return createPortal(
    <TerminalTab
      sessionId={sessionId}
      connected={connected}
      agentCommandName={agentCommandName}
      agentCommandNameCaseInsensitive={agentCommandNameCaseInsensitive}
      readOnly={readOnly}
      forceAutoScroll={forceAutoScroll}
      promptHookEnabled={promptHookEnabled}
      showTimestamps={showTimestamps}
      fontSize={fontSize}
      theme={theme}
      fontFamily={fontFamily}
      onContextMenu={onContextMenu}
      onAgentCommand={onAgentCommand}
      onCloseRequest={onCloseRequest}
      broadcastTargets={broadcastTargets}
    />,
    host,
  );
}
