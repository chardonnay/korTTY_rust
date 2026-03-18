import { useState, useRef, useEffect } from "react";

interface MenuBarProps {
  onNewWindow: () => void;
  onCloseWindow: () => void;
  onNewTab: () => void;
  onCloseTab: () => void;
  onToggleDashboard: () => void;
  onQuickConnect: () => void;
  onManageConnections: () => void;
  onImportConnections: () => void;
  onSettings: () => void;
  onManageCredentials: () => void;
  onManageSSHKeys: () => void;
  onManageGPGKeys: () => void;
  onSnippets: () => void;
  onSFTPManager: () => void;
  onAsciiArt: () => void;
  onCreateBackup: () => void;
  onImportBackup: () => void;
  onTeamworkSettings: () => void;
  onTerminalThemeEditor: () => void;
  onGuiThemeEditor: () => void;
  onFullscreen: () => void;
  onQuit: () => void;
  onAbout: () => void;
}

interface MenuItem {
  label: string;
  shortcut?: string;
  separator?: boolean;
  action?: () => void;
  disabled?: boolean;
}

interface MenuDef {
  label: string;
  items: MenuItem[];
}

export function MenuBar(props: MenuBarProps) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const menus: MenuDef[] = [
    {
      label: "File",
      items: [
        { label: "New Window", shortcut: "Ctrl+Shift+N", action: props.onNewWindow },
        { label: "Close Window", shortcut: "Ctrl+Shift+W", action: props.onCloseWindow },
        { separator: true, label: "" },
        { label: "New Tab", shortcut: "Ctrl+T", action: props.onNewTab },
        { label: "Close Tab", shortcut: "Ctrl+W", action: props.onCloseTab },
        { separator: true, label: "" },
        { label: "Quick Connect...", shortcut: "Ctrl+K", action: props.onQuickConnect },
        { separator: true, label: "" },
        { label: "Settings...", action: props.onSettings },
        { separator: true, label: "" },
        { label: "Quit", shortcut: "Ctrl+Q", action: props.onQuit },
      ],
    },
    {
      label: "Edit",
      items: [
        { label: "Copy", shortcut: "Ctrl+Shift+C" },
        { label: "Paste", shortcut: "Ctrl+Shift+V" },
        { label: "Find...", shortcut: "Ctrl+F" },
        { separator: true, label: "" },
        { label: "Create Backup...", shortcut: "Ctrl+Shift+B", action: props.onCreateBackup },
        { label: "Import Backup...", action: props.onImportBackup },
        { separator: true, label: "" },
        { label: "Terminal Theme...", action: props.onTerminalThemeEditor },
        { label: "GUI Theme...", action: props.onGuiThemeEditor },
      ],
    },
    {
      label: "Connections",
      items: [
        { label: "Manage Connections...", action: props.onManageConnections },
        { separator: true, label: "" },
        { label: "Import...", action: props.onImportConnections },
        { separator: true, label: "" },
        { label: "Teamwork Settings...", action: props.onTeamworkSettings },
      ],
    },
    {
      label: "Management",
      items: [
        { label: "Manage Credentials...", action: props.onManageCredentials },
        { label: "Manage SSH Keys...", action: props.onManageSSHKeys },
        { label: "Manage GPG Keys...", action: props.onManageGPGKeys },
      ],
    },
    {
      label: "Tools",
      items: [
        { label: "Open SFTP Manager...", action: props.onSFTPManager },
        { label: "ASCII Art Banner...", action: props.onAsciiArt },
        { separator: true, label: "" },
        { label: "Snippets...", action: props.onSnippets },
      ],
    },
    {
      label: "View",
      items: [
        { label: "Toggle Dashboard", shortcut: "Ctrl+Shift+D", action: props.onToggleDashboard },
        { separator: true, label: "" },
        { label: "Fullscreen", shortcut: "F11", action: props.onFullscreen },
      ],
    },
    {
      label: "Help",
      items: [
        { label: "About KorTTY", action: props.onAbout },
      ],
    },
  ];

  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpenMenu(null);
      }
    }
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  return (
    <div
      ref={menuRef}
      className="flex items-center h-8 bg-kortty-surface border-b border-kortty-border text-sm select-none"
      data-tauri-drag-region
    >
      <div className="flex items-center gap-0 px-1">
        {menus.map((menu) => (
          <div key={menu.label} className="relative">
            <button
              className={`px-3 py-1 rounded text-xs hover:bg-kortty-panel transition-colors ${
                openMenu === menu.label ? "bg-kortty-panel text-kortty-accent" : "text-kortty-text"
              }`}
              onClick={() => setOpenMenu(openMenu === menu.label ? null : menu.label)}
              onMouseEnter={() => openMenu && setOpenMenu(menu.label)}
            >
              {menu.label}
            </button>
            {openMenu === menu.label && (
              <div className="absolute left-0 top-full mt-0.5 bg-kortty-panel border border-kortty-border rounded-md shadow-xl min-w-[220px] py-1 z-50">
                {menu.items.map((item, idx) =>
                  item.separator ? (
                    <div key={idx} className="my-1 border-t border-kortty-border" />
                  ) : (
                    <button
                      key={idx}
                      className={`w-full flex items-center justify-between px-3 py-1.5 text-xs transition-colors ${
                        item.action
                          ? "hover:bg-kortty-accent/10 hover:text-kortty-accent"
                          : "opacity-40 cursor-not-allowed"
                      }`}
                      disabled={!item.action || item.disabled}
                      onClick={() => {
                        if (item.action) {
                          item.action();
                          setOpenMenu(null);
                        }
                      }}
                    >
                      <span>{item.label}</span>
                      {item.shortcut && (
                        <span className="text-kortty-text-dim ml-6 text-[10px]">{item.shortcut}</span>
                      )}
                    </button>
                  ),
                )}
              </div>
            )}
          </div>
        ))}
      </div>
      <div className="flex-1" data-tauri-drag-region />
    </div>
  );
}
