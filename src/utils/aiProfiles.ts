import type { AiProfile } from "../types/ai";

export function resolvePreferredAiProfileId(
  profiles: AiProfile[],
  defaultProfileId?: string | null,
  currentProfileId?: string | null,
): string {
  if (currentProfileId && profiles.some((profile) => profile.id === currentProfileId)) {
    return currentProfileId;
  }

  if (defaultProfileId && profiles.some((profile) => profile.id === defaultProfileId)) {
    return defaultProfileId;
  }

  return profiles[0]?.id || "";
}
