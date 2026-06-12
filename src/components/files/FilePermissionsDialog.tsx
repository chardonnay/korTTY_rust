import { useEffect, useId, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { RefreshCw, X } from "lucide-react";
import type { LocalFileStat } from "./FilePropertiesDialog";

interface LocalPrincipals {
  users: string[];
  groups: string[];
}

interface OwnerPermissionsResult {
  changed: number;
  failed: number;
  lastError: string;
}

/** Port of LocalFileBrowser.isValidOctalPermissions, extended to 4 digits. */
export function isValidOctalInput(value: string): boolean {
  return /^[0-7]{3,4}$/.test(value);
}

/** Port of LocalFileBrowser.octalToPosix (preview uses the last 3 digits). */
export function octalToPosix(octal: string): string {
  const digits = octal.slice(-3);
  let posix = "";
  for (const char of digits) {
    const value = parseInt(char, 8);
    posix += (value & 4) !== 0 ? "r" : "-";
    posix += (value & 2) !== 0 ? "w" : "-";
    posix += (value & 1) !== 0 ? "x" : "-";
  }
  return posix;
}

interface FilePermissionsDialogProps {
  paths: string[];
  onClose: () => void;
  /** Receives the localized status message after applying changes. */
  onDone: (message: string) => void;
}

/**
 * Owner/group/permissions dialog for local files (port of the Java
 * LocalFileBrowser.setOwnerPermissionsDialog). Owner and group are editable
 * combos backed by /etc/passwd and /etc/group (local_list_principals); the
 * octal field validates live and previews the rwx form.
 */
export function FilePermissionsDialog({ paths, onClose, onDone }: FilePermissionsDialogProps) {
  const { t } = useTranslation();
  const usersListId = useId();
  const groupsListId = useId();

  const [principals, setPrincipals] = useState<LocalPrincipals>({ users: [], groups: [] });
  const [currentOwner, setCurrentOwner] = useState("");
  const [currentGroup, setCurrentGroup] = useState("");
  const [currentOctal, setCurrentOctal] = useState("");
  const [owner, setOwner] = useState("");
  const [group, setGroup] = useState("");
  const [octal, setOctal] = useState("");
  const [recursive, setRecursive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    let active = true;
    invoke<LocalPrincipals>("local_list_principals")
      .then((loaded) => { if (active) setPrincipals(loaded); })
      .catch(() => {});
    if (paths.length > 0) {
      invoke<LocalFileStat>("local_stat", { path: paths[0] })
        .then((stat) => {
          if (!active) return;
          setCurrentOwner(stat.owner);
          setCurrentGroup(stat.group);
          setCurrentOctal(stat.permissionsOctal);
          setOwner(stat.owner);
          setGroup(stat.group);
          setOctal(stat.permissionsOctal);
        })
        .catch(() => {});
    }
    return () => { active = false; };
  }, [paths]);

  const octalTrimmed = octal.trim();
  const octalValid = octalTrimmed === "" || isValidOctalInput(octalTrimmed);
  const ownerChanged = owner.trim() !== "" && owner.trim() !== currentOwner;
  const groupChanged = group.trim() !== "" && group.trim() !== currentGroup;
  const octalChanged = octalTrimmed !== "" && octalTrimmed !== currentOctal;
  const hasChanges = ownerChanged || groupChanged || octalChanged;

  const ownerOptions = useMemo(() => {
    const values = currentOwner && !principals.users.includes(currentOwner)
      ? [currentOwner, ...principals.users]
      : principals.users;
    return values;
  }, [currentOwner, principals.users]);

  const groupOptions = useMemo(() => {
    const values = currentGroup && !principals.groups.includes(currentGroup)
      ? [currentGroup, ...principals.groups]
      : principals.groups;
    return values;
  }, [currentGroup, principals.groups]);

  const apply = async () => {
    if (!hasChanges || !octalValid || busy) return;
    setBusy(true);
    setError("");
    try {
      const result = await invoke<OwnerPermissionsResult>("local_set_owner_permissions", {
        paths,
        owner: ownerChanged ? owner.trim() : null,
        group: groupChanged ? group.trim() : null,
        octal: octalChanged ? octalTrimmed : null,
        recursive,
      });
      if (result.failed === 0) {
        onDone(t("filebrowser.setOwner.success", { count: result.changed }));
      } else {
        const message = t("filebrowser.setOwner.errorCount", {
          failed: result.failed,
          total: paths.length,
        });
        onDone(result.lastError ? `${message} ${result.lastError}` : message);
      }
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60">
      <div className="w-[420px] rounded-lg border border-kortty-border bg-kortty-bg shadow-2xl">
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h3 className="text-sm font-semibold text-kortty-text">{t("filebrowser.setOwner.title")}</h3>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-3 p-4">
          <div className="text-xs text-kortty-text-dim">
            {t("filebrowser.setOwner.header", { count: paths.length })}
          </div>
          <div>
            <label className="mb-1 block text-xs text-kortty-text-dim">
              {t("filebrowser.setOwner.ownerUser")}
            </label>
            <input
              value={owner}
              onChange={(event) => setOwner(event.target.value)}
              list={usersListId}
              placeholder="user"
              className="w-full rounded border border-kortty-border bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text"
            />
            <datalist id={usersListId}>
              {ownerOptions.map((value) => <option key={value} value={value} />)}
            </datalist>
          </div>
          <div>
            <label className="mb-1 block text-xs text-kortty-text-dim">
              {t("filebrowser.setOwner.ownerGroup")}
            </label>
            <input
              value={group}
              onChange={(event) => setGroup(event.target.value)}
              list={groupsListId}
              placeholder="group"
              className="w-full rounded border border-kortty-border bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text"
            />
            <datalist id={groupsListId}>
              {groupOptions.map((value) => <option key={value} value={value} />)}
            </datalist>
          </div>
          <div>
            <label className="mb-1 block text-xs text-kortty-text-dim">
              {t("filebrowser.setOwner.permissions")}
            </label>
            <div className="flex items-center gap-2">
              <input
                value={octal}
                onChange={(event) => setOctal(event.target.value)}
                placeholder="755"
                maxLength={4}
                className={`w-24 rounded border bg-kortty-panel px-2 py-1.5 font-mono text-xs text-kortty-text ${
                  octalValid ? "border-kortty-border" : "border-kortty-error"
                }`}
              />
              <span className={`font-mono text-xs ${octalValid ? "text-kortty-text-dim" : "text-kortty-error"}`}>
                {octalTrimmed === ""
                  ? ""
                  : octalValid
                    ? octalToPosix(octalTrimmed)
                    : t("filebrowser.setOwner.invalid")}
              </span>
            </div>
          </div>
          <label className="flex cursor-pointer items-center gap-2 text-xs text-kortty-text">
            <input
              type="checkbox"
              checked={recursive}
              onChange={(event) => setRecursive(event.target.checked)}
              className="rounded border-kortty-border"
            />
            {t("filebrowser.setOwner.recursive")}
          </label>
          <div className="whitespace-pre-line text-[11px] text-kortty-text-dim">
            {t("filebrowser.setOwner.infoSeparate")}
          </div>
          {error && <div className="text-xs text-kortty-error">{error}</div>}
        </div>
        <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
          <button
            onClick={onClose}
            className="rounded border border-kortty-border px-4 py-1.5 text-xs text-kortty-text hover:bg-kortty-panel"
          >
            {t("common.cancel")}
          </button>
          <button
            onClick={() => void apply()}
            disabled={busy || !hasChanges || !octalValid}
            className="flex items-center gap-1.5 rounded bg-kortty-accent px-4 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover disabled:opacity-40"
          >
            {busy && <RefreshCw className="h-3 w-3 animate-spin" />}
            {t("common.ok")}
          </button>
        </div>
      </div>
    </div>
  );
}
