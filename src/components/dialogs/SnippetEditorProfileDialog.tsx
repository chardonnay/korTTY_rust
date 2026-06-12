import { useEffect, useMemo, useState } from "react";
import { X, Palette, Plus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { SnippetEditorCursorStyle, SnippetEditorProfile } from "../../types/snippet";
import {
  builtInProfiles,
  hexColor,
  normalizeCursorStyle,
  normalizeProfile,
} from "../../utils/snippetEditorProfiles";

// Dialog for managing snippet editor color profiles. Built-in profiles are
// read-only, custom profiles support full CRUD and are persisted by the host
// through onSaveProfiles (GlobalSettings.snippetEditorProfiles).
// Port of de.kortty.ui.SnippetEditorProfileDialog.

export interface SnippetEditorProfileDialogProps {
  open: boolean;
  onClose: () => void;
  customProfiles: SnippetEditorProfile[];
  onSaveProfiles: (profiles: SnippetEditorProfile[]) => void | Promise<void>;
  selectedProfileId?: string;
  onSelectProfile?: (id: string | undefined) => void;
}

const CURSOR_STYLES: SnippetEditorCursorStyle[] = ["BLOCK", "LINE", "UNDERSCORE"];

type ColorField =
  | "foregroundColor"
  | "backgroundColor"
  | "cursorColor"
  | "commentColor"
  | "stringColor"
  | "numberColor"
  | "booleanColor"
  | "keyColor"
  | "keywordColor"
  | "sectionColor"
  | "variableColor"
  | "braceColor";

const COLOR_FIELDS: { field: ColorField; labelKey: string }[] = [
  { field: "foregroundColor", labelKey: "settings.snippetEditor.foreground" },
  { field: "backgroundColor", labelKey: "settings.snippetEditor.background" },
  { field: "cursorColor", labelKey: "settings.snippetEditor.cursorColor" },
  { field: "commentColor", labelKey: "snippet.profile.comment" },
  { field: "stringColor", labelKey: "snippet.profile.string" },
  { field: "numberColor", labelKey: "snippet.profile.number" },
  { field: "booleanColor", labelKey: "snippet.profile.boolean" },
  { field: "keyColor", labelKey: "snippet.profile.key" },
  { field: "keywordColor", labelKey: "snippet.profile.keyword" },
  { field: "sectionColor", labelKey: "snippet.profile.section" },
  { field: "variableColor", labelKey: "snippet.profile.variable" },
  { field: "braceColor", labelKey: "snippet.profile.brace" },
];

function ProfilePreview({ profile }: { profile: SnippetEditorProfile }) {
  const normalized = normalizeProfile(profile);
  return (
    <pre
      className="overflow-x-auto rounded border border-kortty-border p-3 font-mono text-xs leading-5"
      style={{ backgroundColor: normalized.backgroundColor, color: normalized.foregroundColor }}
    >
      <span style={{ color: normalized.commentColor }}># backup rotation example{"\n"}</span>
      <span style={{ color: normalized.sectionColor }}>[backup]{"\n"}</span>
      <span style={{ color: normalized.keywordColor }}>if</span>
      {" [ "}
      <span style={{ color: normalized.variableColor }}>$COUNT</span>
      {" -gt "}
      <span style={{ color: normalized.numberColor }}>7</span>
      {" ]; "}
      <span style={{ color: normalized.keywordColor }}>then{"\n"}</span>
      {"  "}
      <span style={{ color: normalized.keyColor }}>target</span>
      <span style={{ color: normalized.braceColor }}>=</span>
      <span style={{ color: normalized.stringColor }}>"/var/backups"</span>
      {"  "}
      <span style={{ color: normalized.booleanColor }}>true</span>
      {"\n"}
      <span style={{ color: normalized.keywordColor }}>fi</span>
      <span
        style={{
          backgroundColor: normalized.cursorColor,
          color: normalized.backgroundColor,
          marginLeft: 2,
        }}
      >
        {profile.cursorStyle === "UNDERSCORE" ? "_" : " "}
      </span>
    </pre>
  );
}

export function SnippetEditorProfileDialog({
  open,
  onClose,
  customProfiles,
  onSaveProfiles,
  selectedProfileId,
  onSelectProfile,
}: SnippetEditorProfileDialogProps) {
  const { t } = useTranslation();
  const builtIns = useMemo(() => builtInProfiles(), []);
  const [drafts, setDrafts] = useState<SnippetEditorProfile[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!open) return;
    const normalized = customProfiles
      .filter((profile) => !profile.builtIn)
      .map((profile) => normalizeProfile(profile));
    setDrafts(normalized);
    const initial =
      (selectedProfileId &&
        [...normalized, ...builtIns].find((profile) => profile.id === selectedProfileId)?.id) ||
      normalized[0]?.id ||
      builtIns[0]?.id ||
      null;
    setSelectedId(initial);
    // Re-initialize the draft list every time the dialog opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  if (!open) return null;

  const selected =
    drafts.find((profile) => profile.id === selectedId) ??
    builtIns.find((profile) => profile.id === selectedId) ??
    null;
  const selectedIsCustom = !!selected && !selected.builtIn;

  function updateSelected(partial: Partial<SnippetEditorProfile>) {
    if (!selectedId) return;
    setDrafts((current) =>
      current.map((profile) => (profile.id === selectedId ? { ...profile, ...partial } : profile)),
    );
  }

  function handleNewProfile() {
    const template = selected ?? builtIns[1] ?? builtIns[0];
    const created = normalizeProfile({
      ...template,
      id: crypto.randomUUID(),
      name: "",
      builtIn: false,
    });
    setDrafts((current) => [...current, created]);
    setSelectedId(created.id);
  }

  function handleDeleteProfile() {
    if (!selectedId || !selectedIsCustom) return;
    setDrafts((current) => current.filter((profile) => profile.id !== selectedId));
    setSelectedId((current) => {
      const remaining = drafts.filter((profile) => profile.id !== current);
      return remaining[0]?.id ?? builtIns[0]?.id ?? null;
    });
  }

  const hasInvalidName = drafts.some((profile) => !profile.name.trim());

  async function handleSave() {
    setSaving(true);
    try {
      const normalized = drafts.map((profile) =>
        normalizeProfile({ ...profile, builtIn: false }),
      );
      await onSaveProfiles(normalized);
      if (
        onSelectProfile &&
        selectedProfileId &&
        ![...normalized, ...builtIns].some((profile) => profile.id === selectedProfileId)
      ) {
        // The active profile was deleted — let the host clear the selection.
        onSelectProfile(undefined);
      }
      onClose();
    } catch (error) {
      console.error("Failed to save snippet editor profiles:", error);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
      <div
        className="flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width: "min(760px, 95vw)", height: "min(620px, 90vh)" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold text-kortty-text">
            <Palette className="h-4 w-4 text-kortty-accent" />
            {t("snippet.profile.title")}
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1">
          <div className="flex w-[220px] flex-col border-r border-kortty-border">
            <div className="flex items-center gap-1 border-b border-kortty-border p-2">
              <button
                className="flex items-center gap-1 rounded bg-kortty-accent px-2 py-1 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover"
                onClick={handleNewProfile}
                type="button"
                title={t("snippet.profile.new")}
              >
                <Plus className="h-3 w-3" />
                {t("common.add")}
              </button>
              <button
                className="flex items-center gap-1 rounded bg-kortty-panel px-2 py-1 text-xs text-kortty-error transition-colors hover:bg-kortty-border disabled:opacity-40"
                onClick={handleDeleteProfile}
                disabled={!selectedIsCustom}
                type="button"
              >
                <Trash2 className="h-3 w-3" />
                {t("common.delete")}
              </button>
            </div>
            <div className="flex-1 overflow-y-auto p-2">
              <div className="mb-1 text-[10px] uppercase tracking-wide text-kortty-text-dim">
                {t("snippet.profile.custom")}
              </div>
              {drafts.map((profile) => (
                <button
                  key={profile.id}
                  type="button"
                  className={`mb-0.5 block w-full truncate rounded px-2 py-1 text-left text-xs ${
                    selectedId === profile.id
                      ? "bg-kortty-accent/10 text-kortty-accent"
                      : "text-kortty-text hover:bg-kortty-panel"
                  }`}
                  onClick={() => setSelectedId(profile.id)}
                >
                  {profile.name.trim() || t("snippet.profile.namePrompt")}
                </button>
              ))}
              {drafts.length === 0 && (
                <div className="px-2 py-1 text-xs text-kortty-text-dim">—</div>
              )}
              <div className="mb-1 mt-3 text-[10px] uppercase tracking-wide text-kortty-text-dim">
                {t("snippet.profile.presets")}
              </div>
              {builtIns.map((profile) => (
                <button
                  key={profile.id}
                  type="button"
                  className={`mb-0.5 block w-full truncate rounded px-2 py-1 text-left text-xs ${
                    selectedId === profile.id
                      ? "bg-kortty-accent/10 text-kortty-accent"
                      : "text-kortty-text hover:bg-kortty-panel"
                  }`}
                  onClick={() => setSelectedId(profile.id)}
                >
                  {profile.name}
                </button>
              ))}
            </div>
          </div>

          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-4">
            {selected ? (
              <div className="space-y-3">
                {!selectedIsCustom && (
                  <div className="rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text-dim">
                    {t("snippet.profile.builtInHint")}
                  </div>
                )}
                <div>
                  <label className="mb-1 block text-xs text-kortty-text-dim">
                    {t("snippet.name")}
                  </label>
                  <input
                    className="input-field"
                    value={selected.name}
                    placeholder={t("snippet.profile.namePrompt")}
                    disabled={!selectedIsCustom}
                    onChange={(event) => updateSelected({ name: event.target.value })}
                  />
                </div>
                <div>
                  <label className="mb-1 block text-xs text-kortty-text-dim">
                    {t("settings.snippetEditor.cursorStyle")}
                  </label>
                  <select
                    className="input-field"
                    value={normalizeCursorStyle(selected.cursorStyle)}
                    disabled={!selectedIsCustom}
                    onChange={(event) =>
                      updateSelected({ cursorStyle: event.target.value as SnippetEditorCursorStyle })
                    }
                  >
                    {CURSOR_STYLES.map((style) => (
                      <option key={style} value={style}>
                        {style}
                      </option>
                    ))}
                  </select>
                </div>
                <div className="grid grid-cols-2 gap-x-4 gap-y-2">
                  {COLOR_FIELDS.map(({ field, labelKey }) => (
                    <label key={field} className="flex items-center justify-between gap-2 text-xs text-kortty-text">
                      <span className="text-kortty-text-dim">{t(labelKey)}</span>
                      <input
                        type="color"
                        className="h-6 w-12 cursor-pointer rounded border border-kortty-border bg-transparent"
                        value={hexColor(selected[field], "#000000").toLowerCase()}
                        disabled={!selectedIsCustom}
                        onChange={(event) =>
                          updateSelected({ [field]: hexColor(event.target.value, "#000000") })
                        }
                      />
                    </label>
                  ))}
                </div>
                <div>
                  <div className="mb-1 text-xs text-kortty-text-dim">
                    {t("snippet.profile.preview")}
                  </div>
                  <ProfilePreview profile={selected} />
                </div>
              </div>
            ) : (
              <div className="py-8 text-center text-xs text-kortty-text-dim">
                {t("snippet.profile.namePrompt")}
              </div>
            )}
          </div>
        </div>

        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
            onClick={onClose}
            type="button"
          >
            {t("common.cancel")}
          </button>
          <button
            className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
            onClick={() => void handleSave()}
            disabled={saving || hasInvalidName}
            type="button"
          >
            {t("common.save")}
          </button>
        </div>
      </div>
    </div>
  );
}
