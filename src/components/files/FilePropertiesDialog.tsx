import { useTranslation } from "react-i18next";
import { X } from "lucide-react";

/** Mirror of the Rust LocalFileStat struct (local_stat command). */
export interface LocalFileStat {
  name: string;
  path: string;
  size: number;
  modified?: string | null;
  permissionsOctal: string;
  permissionsRwx: string;
  owner: string;
  group: string;
  isDir: boolean;
}

/** Port of LocalFileBrowser.formatSize (Java). */
export function formatFileSize(size: number): string {
  if (size < 0) return "0 B";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`;
  return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

interface FilePropertiesDialogProps {
  stats: LocalFileStat[];
  onClose: () => void;
}

/**
 * Details dialog for one or more local files (port of the Java
 * LocalFileBrowser.showDetails alert).
 */
export function FilePropertiesDialog({ stats, onClose }: FilePropertiesDialogProps) {
  const { t } = useTranslation();
  if (stats.length === 0) return null;

  const header = stats.length === 1
    ? stats[0].name
    : `${stats.length} ${t("filebrowser.selected")}`;

  const Row = ({ label, value, mono }: { label: string; value: string; mono?: boolean }) => (
    <div className="flex gap-2 text-xs">
      <span className="w-28 shrink-0 text-kortty-text-dim">{label}</span>
      <span className={`min-w-0 break-all text-kortty-text ${mono ? "font-mono" : ""}`}>{value}</span>
    </div>
  );

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60">
      <div className="w-[440px] max-h-[70vh] flex flex-col rounded-lg border border-kortty-border bg-kortty-bg shadow-2xl">
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h3 className="min-w-0 truncate text-sm font-semibold text-kortty-text">
            {t("filebrowser.context.details")} — {header}
          </h3>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="min-h-0 flex-1 space-y-4 overflow-auto p-4">
          {stats.map((stat) => (
            <div key={stat.path} className="space-y-1.5">
              {stats.length > 1 && (
                <div className="truncate text-xs font-semibold text-kortty-text">{stat.name}</div>
              )}
              <Row
                label={t("filebrowser.details.type")}
                value={stat.isDir ? t("filebrowser.details.folder") : t("filebrowser.details.file")}
              />
              {!stat.isDir && <Row label={t("filebrowser.details.size")} value={formatFileSize(stat.size)} />}
              <Row label={t("filebrowser.details.path")} value={stat.path} mono />
              {stat.modified && <Row label={t("filebrowser.details.modified")} value={stat.modified} />}
              <Row
                label={t("filebrowser.details.permissions")}
                value={
                  stat.permissionsOctal !== ""
                    ? `${stat.permissionsRwx} (${stat.permissionsOctal})`
                    : stat.permissionsRwx
                }
                mono
              />
              {stat.owner !== "" && <Row label={t("filebrowser.details.owner")} value={stat.owner} />}
              {stat.group !== "" && <Row label={t("filebrowser.details.group")} value={stat.group} />}
            </div>
          ))}
        </div>
        <div className="flex justify-end border-t border-kortty-border px-4 py-3">
          <button
            onClick={onClose}
            className="rounded bg-kortty-accent px-6 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover"
          >
            {t("common.ok")}
          </button>
        </div>
      </div>
    </div>
  );
}
