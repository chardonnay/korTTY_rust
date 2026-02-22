interface StatusBarProps {
  connectionCount: number;
}

export function StatusBar({ connectionCount }: StatusBarProps) {
  return (
    <div className="flex items-center justify-between h-6 px-3 bg-kortty-surface border-t border-kortty-border text-[10px] text-kortty-text-dim select-none">
      <div className="flex items-center gap-3">
        <span>KorTTY v1.0.0</span>
        <span>
          {connectionCount} connection{connectionCount !== 1 ? "s" : ""}
        </span>
      </div>
      <div className="flex items-center gap-3">
        <span>{connectionCount > 0 ? "Connected" : "Ready"}</span>
      </div>
    </div>
  );
}
