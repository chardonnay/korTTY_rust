import { useState, useEffect } from "react";
import { X, Type, Copy } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";

interface AsciiArtBannerProps {
  open: boolean;
  onClose: () => void;
}

const FONTS = [
  "standard",
  "3-D",
  "digital",
  "lean",
  "banner",
  "big",
  "block",
  "cosmic",
  "roman",
  "script",
  "small",
];

export function AsciiArtBanner({ open, onClose }: AsciiArtBannerProps) {
  const { width, height, onResizeStart } = useDialogGeometry("ascii-art", 600, 500, 400, 300);
  const [text, setText] = useState("KorTTY");
  const [font, setFont] = useState("standard");
  const [preview, setPreview] = useState("");
  const [fonts, setFonts] = useState<string[]>(FONTS);
  const [generating, setGenerating] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (open) {
      invoke<string[]>("get_font_list")
        .then(setFonts)
        .catch(() => setFonts(FONTS));
    }
  }, [open]);

  useEffect(() => {
    if (!open || !text) {
      setPreview("");
      return;
    }
    setGenerating(true);
    invoke<string>("generate_banner", { text, font })
      .then(setPreview)
      .catch((err) => setPreview(`Error: ${String(err)}`))
      .finally(() => setGenerating(false));
  }, [open, text, font]);

  async function handleCopy() {
    if (!preview) return;
    try {
      await navigator.clipboard.writeText(preview);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error("Failed to copy:", err);
    }
  }

  if (!open) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Type className="w-4 h-4 text-kortty-accent" />
            FIGlet Banner
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="p-4 space-y-3">
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Text</label>
            <input
              className="input-field"
              value={text}
              onChange={(e) => setText(e.target.value)}
              placeholder="Enter text"
            />
          </div>
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Font</label>
            <select
              className="input-field"
              value={font}
              onChange={(e) => setFont(e.target.value)}
            >
              {fonts.map((f) => (
                <option key={f} value={f}>
                  {f}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label className="block text-xs text-kortty-text-dim mb-1">Preview</label>
            <pre
              className="w-full min-h-[100px] max-h-[240px] p-3 bg-kortty-bg border border-kortty-border rounded font-mono text-xs text-kortty-text overflow-auto whitespace-pre"
              style={{ fontFamily: "JetBrains Mono, monospace" }}
            >
              {generating ? "Generating…" : preview || "Enter text to preview"}
            </pre>
          </div>
          <button
            className="flex items-center gap-2 px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
            onClick={handleCopy}
            disabled={!preview || generating}
          >
            <Copy className="w-3 h-3" />
            {copied ? "Copied!" : "Copy to Clipboard"}
          </button>
        </div>
        <div
          className="absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize opacity-40 hover:opacity-100 transition-opacity"
          onMouseDown={onResizeStart}
        >
          <svg viewBox="0 0 16 16" className="w-full h-full text-kortty-text-dim">
            <path d="M14 14L8 14L14 8Z" fill="currentColor" />
            <path d="M14 14L11 14L14 11Z" fill="currentColor" opacity="0.5" />
          </svg>
        </div>
      </div>
    </div>
  );
}
