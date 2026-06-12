import { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

export interface TerminalEmulationInfo {
  name: string;
  termName: string;
  displayName: string;
}

/** Default emulation, mirrors the backend XTERM entry. */
const DEFAULT_EMULATION: TerminalEmulationInfo = {
  name: "XTERM",
  termName: "xterm-256color",
  displayName: "XTerm (256 color)",
};

let cachedEmulations: TerminalEmulationInfo[] | null = null;

function normalize(value: string): string {
  return value.trim().toLowerCase();
}

/** Combined display label, mirrors Java TerminalEmulationSupport.displayName. */
function displayLabel(emulation: TerminalEmulationInfo): string {
  return `${emulation.displayName} (${emulation.termName})`;
}

/** Exact normalized match against enum name, term name, display name or label. */
function findExact(
  emulations: TerminalEmulationInfo[],
  input: string,
): TerminalEmulationInfo | undefined {
  const normalized = normalize(input);
  if (!normalized) return undefined;
  return emulations.find(
    (emulation) =>
      normalize(emulation.name) === normalized ||
      normalize(emulation.termName) === normalized ||
      normalize(emulation.displayName) === normalized ||
      normalize(displayLabel(emulation)) === normalized,
  );
}

/** Substring search, mirrors Java TerminalEmulationSupport.matchesSearch. */
function matchesSearch(emulation: TerminalEmulationInfo, query: string): boolean {
  const normalized = normalize(query);
  if (!normalized) return true;
  return (
    normalize(emulation.name).includes(normalized) ||
    normalize(emulation.termName).includes(normalized) ||
    normalize(emulation.displayName).includes(normalized) ||
    normalize(displayLabel(emulation)).includes(normalized)
  );
}

interface TerminalEmulationSelectProps {
  /** Stored enum name of the selected emulation (e.g. "XTERM", "VT220"). */
  value?: string;
  /** Called with the enum name when a different emulation is selected. */
  onChange: (name: string) => void;
  className?: string;
}

/**
 * Searchable terminal emulation combo box. Typing filters the list across
 * enum name, term name and display name; entries render as
 * "DisplayName (termname)" and the selection is stored as the enum name.
 * Behavior modeled on the Java TerminalEmulationComboBoxSupport.
 */
export function TerminalEmulationSelect({ value, onChange, className = "" }: TerminalEmulationSelectProps) {
  const [emulations, setEmulations] = useState<TerminalEmulationInfo[]>(
    cachedEmulations ?? [DEFAULT_EMULATION],
  );
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (cachedEmulations) return;
    invoke<TerminalEmulationInfo[]>("get_terminal_emulations")
      .then((list) => {
        if (list.length > 0) {
          cachedEmulations = list;
          setEmulations(list);
        }
      })
      .catch(console.error);
  }, []);

  const selected = useMemo(
    () => findExact(emulations, value || "") ?? emulations[0] ?? DEFAULT_EMULATION,
    [emulations, value],
  );

  const filtered = useMemo(
    () => emulations.filter((emulation) => matchesSearch(emulation, query)),
    [emulations, query],
  );

  function openDropdown() {
    // Opening clears the filter so the full list is visible (Java behavior).
    setQuery("");
    setOpen(true);
  }

  function closeAndNormalize(commitText?: string) {
    if (commitText !== undefined) {
      const match = findExact(emulations, commitText);
      if (match && match.name !== selected.name) {
        onChange(match.name);
      }
    }
    setQuery("");
    setOpen(false);
  }

  function select(emulation: TerminalEmulationInfo) {
    if (emulation.name !== selected.name) {
      onChange(emulation.name);
    }
    setQuery("");
    setOpen(false);
  }

  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
        closeAndNormalize();
      }
    }
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return (
    <div ref={containerRef} className={`relative ${className}`}>
      <div className="relative">
        <input
          ref={inputRef}
          className="input-field w-full pr-7"
          value={open ? query : displayLabel(selected)}
          placeholder={displayLabel(selected)}
          onFocus={openDropdown}
          onClick={() => {
            if (!open) openDropdown();
          }}
          onChange={(e) => {
            if (!open) setOpen(true);
            setQuery(e.target.value);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              const match = findExact(emulations, query);
              if (match) {
                select(match);
              } else if (filtered.length === 1) {
                select(filtered[0]);
              } else {
                closeAndNormalize();
              }
            } else if (e.key === "Escape") {
              closeAndNormalize();
            }
          }}
        />
        <button
          type="button"
          tabIndex={-1}
          className="absolute right-1.5 top-1/2 -translate-y-1/2 text-kortty-text-dim hover:text-kortty-text"
          onMouseDown={(e) => {
            e.preventDefault();
            if (open) {
              closeAndNormalize();
            } else {
              openDropdown();
              inputRef.current?.focus();
            }
          }}
        >
          <ChevronDown className="w-3.5 h-3.5" />
        </button>
      </div>
      {open && (
        <div className="absolute z-50 mt-1 w-full max-h-48 overflow-y-auto bg-kortty-surface border border-kortty-border rounded shadow-xl">
          {filtered.length === 0 ? (
            <div className="px-2.5 py-1.5 text-xs text-kortty-text-dim">—</div>
          ) : (
            filtered.map((emulation) => (
              <button
                key={emulation.name}
                type="button"
                className={`block w-full text-left px-2.5 py-1.5 text-xs transition-colors ${
                  emulation.name === selected.name
                    ? "bg-kortty-accent/10 text-kortty-accent"
                    : "text-kortty-text hover:bg-kortty-panel"
                }`}
                onMouseDown={(e) => {
                  e.preventDefault();
                  select(emulation);
                }}
              >
                {displayLabel(emulation)}
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}
