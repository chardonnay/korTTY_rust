export type UpdateCheckStatus =
  | "UpdateAvailable"
  | "NoUpdate"
  | "NoCompatibleAsset"
  | "Failed";

export type UpdateCheckRunType = "Manual" | "AutomaticStartup" | "AutomaticPeriodic";

export interface UpdateAsset {
  name: string;
  downloadUri: string;
  size: number;
  digest?: string;
}

export interface UpdateRelease {
  tagName: string;
  name: string;
  htmlUri?: string;
  /** ISO-8601 timestamp of the release publication. */
  publishedAt?: string;
  draft: boolean;
  prerelease: boolean;
  assets: UpdateAsset[];
}

export interface AvailableUpdate {
  release: UpdateRelease;
  asset: UpdateAsset;
  latestVersion: string;
  currentVersion: string;
}

export interface UpdateCheckResult {
  status: UpdateCheckStatus;
  update?: AvailableUpdate;
  message?: string;
}

/** Payload of the `update://download-progress` event; `bytesTotal` is -1 when unknown. */
export interface UpdateDownloadProgress {
  bytesDone: number;
  bytesTotal: number;
}

/** Emitted with an {@link AvailableUpdate} payload when an automatic check finds an update. */
export const UPDATE_AVAILABLE_EVENT = "update://available";

/** Emitted with an {@link UpdateDownloadProgress} payload while an asset download runs. */
export const UPDATE_DOWNLOAD_PROGRESS_EVENT = "update://download-progress";
