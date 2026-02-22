import { useState, useRef, useEffect } from "react";

interface MenuBarProps {
  onToggleDashboard: () => void;
  onNewTab: () => void;
  onConnect?: (tabId: string, host: string, port: number, username: string, password: string) => void;
  activeTabId?: string | null;
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

export function MenuBar({ onToggleDashboard, onNewTab, onConnect, activeTabId }: MenuBarProps) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const menus: MenuDef[] = [
    {
      label: "File",
      items: [
        { label: "New Tab", shortcut: "Ctrl+T", action: onNewTab },
        { label: "New Window", shortcut: "Ctrl+Shift+N" },
        { label: "Close Tab", shortcut: "Ctrl+W" },
        { separator: true, label: "" },
        { label: "Quick Connect...", shortcut: "Ctrl+K" },
        { separator: true, label: "" },
        { label: "Open Project...", shortcut: "Ctrl+O" },
        { label: "Save Project", shortcut: "Ctrl+S" },
        { label: "Save Project As..." },
        { separator: true, label: "" },
        { label: "Quit", shortcut: "Ctrl+Q" },
      ],
    },
    {
      label: "Edit",
      items: [
        { label: "Copy", shortcut: "Ctrl+Shift+C" },
        { label: "Paste", shortcut: "Ctrl+Shift+V" },
        { label: "Find...", shortcut: "Ctrl+F" },
        { separator: true, label: "" },
        { label: "Create Backup...", shortcut: "Ctrl+Shift+B" },
        { label: "Import Backup..." },
        { separator: true, label: "" },
        { label: "Settings..." },
      ],
    },
    {
      label: "Connections",
      items: [
        { label: "Manage Connections..." },
        { separator: true, label: "" },
        { label: "Import..." },
        { label: "Export..." },
      ],
    },
    {
      label: "Management",
      items: [
        { label: "Manage Credentials..." },
        { label: "Manage SSH Keys..." },
        { label: "Manage GPG Keys..." },
        { separator: true, label: "" },
        { label: "Snippets..." },
      ],
    },
    {
      label: "Tools",
      items: [
        { label: "Open SFTP Manager..." },
        { label: "ASCII Art Banner..." },
        { separator: true, label: "" },
        { label: "Teamwork Settings..." },
      ],
    },
    {
      label: "View",
      items: [
        { label: "Toggle Dashboard", shortcut: "Ctrl+Shift+D", action: onToggleDashboard },
        { separator: true, label: "" },
        { label: "Zoom In", shortcut: "Ctrl+=" },
        { label: "Zoom Out", shortcut: "Ctrl+-" },
        { label: "Reset Zoom", shortcut: "Ctrl+0" },
        { separator: true, label: "" },
        { label: "Fullscreen", shortcut: "F11" },
      ],
    },
    {
      label: "Help",
      items: [
        { label: "User Guide" },
        { label: "Keyboard Shortcuts" },
        { separator: true, label: "" },
        { label: "About KorTTY" },
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
                      className="w-full flex items-center justify-between px-3 py-1.5 text-xs hover:bg-kortty-accent/10 hover:text-kortty-accent disabled:opacity-40 transition-colors"
                      disabled={item.disabled}
                      onClick={() => {
                        item.action?.();
                        setOpenMenu(null);
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
