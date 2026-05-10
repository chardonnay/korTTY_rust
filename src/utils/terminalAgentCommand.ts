export const DEFAULT_TERMINAL_AGENT_COMMAND_NAME = "agent";

const TERMINAL_AGENT_COMMAND_NAME_PATTERN = /^[A-Za-z][A-Za-z0-9_-]*$/;

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function normalizeTerminalAgentCommandName(value?: string | null): string {
  const trimmed = value?.trim() ?? "";
  return trimmed || DEFAULT_TERMINAL_AGENT_COMMAND_NAME;
}

export function getTerminalAgentAskCommandName(commandName?: string | null): string {
  return `${normalizeTerminalAgentCommandName(commandName)}-ask`;
}

export function getTerminalAgentPlanCommandName(commandName?: string | null): string {
  return `${normalizeTerminalAgentCommandName(commandName)}-plan`;
}

export function getTerminalAgentCommandNameValidationMessage(value?: string | null): string | null {
  const trimmed = value?.trim() ?? "";
  if (!trimmed) {
    return null;
  }
  if (TERMINAL_AGENT_COMMAND_NAME_PATTERN.test(trimmed)) {
    return null;
  }
  return "Use a single command name that starts with a letter and contains only letters, numbers, `-`, or `_`.";
}

function commandFlags(caseInsensitive = false): string {
  return caseInsensitive ? "i" : "";
}

export function buildTerminalAgentCommandPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedName = escapeRegExp(normalizeTerminalAgentCommandName(commandName));
  return new RegExp(
    `^${escapedName}(?:\\s*\\((.*)\\))?(?:(?:\\s*:\\s*)|\\s+)(.+)$`,
    commandFlags(caseInsensitive),
  );
}

export function buildTerminalAgentAskPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedAskName = escapeRegExp(getTerminalAgentAskCommandName(commandName));
  return new RegExp(
    `^${escapedAskName}(?:(?:\\s*:\\s*)|\\s+)(.+)$`,
    commandFlags(caseInsensitive),
  );
}

export function buildTerminalAgentAskPrefixPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedAskName = escapeRegExp(getTerminalAgentAskCommandName(commandName));
  return new RegExp(`^${escapedAskName}\\b`, commandFlags(caseInsensitive));
}

export function buildTerminalAgentPlanPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedPlanName = escapeRegExp(getTerminalAgentPlanCommandName(commandName));
  return new RegExp(
    `^${escapedPlanName}(?:\\s*\\((.*)\\))?(?:(?:\\s*:\\s*)|\\s+)(.+)$`,
    commandFlags(caseInsensitive),
  );
}

export function buildTerminalAgentPlanPrefixPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedPlanName = escapeRegExp(getTerminalAgentPlanCommandName(commandName));
  return new RegExp(`^${escapedPlanName}\\b`, commandFlags(caseInsensitive));
}

export function buildTerminalAgentShortcutCommandPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedName = escapeRegExp(normalizeTerminalAgentCommandName(commandName));
  const escapedAskName = escapeRegExp(getTerminalAgentAskCommandName(commandName));
  const escapedPlanName = escapeRegExp(getTerminalAgentPlanCommandName(commandName));
  return new RegExp(
    `^(?:${escapedName}(?:\\s*\\([^)]*\\))?(?:(?:\\s*:\\s*)|\\s+).+|${escapedAskName}(?:(?:\\s*:\\s*)|\\s+).+|${escapedPlanName}(?:\\s*\\([^)]*\\))?(?:(?:\\s*:\\s*)|\\s+).+)$`,
    commandFlags(caseInsensitive),
  );
}

export function buildTerminalAgentPromptLineExtractPattern(commandName: string, caseInsensitive = false): RegExp {
  const escapedName = escapeRegExp(normalizeTerminalAgentCommandName(commandName));
  const escapedAskName = escapeRegExp(getTerminalAgentAskCommandName(commandName));
  const escapedPlanName = escapeRegExp(getTerminalAgentPlanCommandName(commandName));
  return new RegExp(
    `^(?:.+?(?:[$#%>❯➜]\\s+)|PS [^>]*>\\s+)?((?:${escapedName}(?:\\s*\\([^)]*\\))?(?:(?:\\s*:\\s*)|\\s+).+|${escapedAskName}(?:(?:\\s*:\\s*)|\\s+).+|${escapedPlanName}(?:\\s*\\([^)]*\\))?(?:(?:\\s*:\\s*)|\\s+).+))$`,
    commandFlags(caseInsensitive),
  );
}

export function buildTerminalAgentUsageText(commandName: string): string {
  const normalizedName = normalizeTerminalAgentCommandName(commandName);
  const askName = getTerminalAgentAskCommandName(commandName);
  const planName = getTerminalAgentPlanCommandName(commandName);
  return `Use \`${normalizedName} <prompt>\`, \`${normalizedName}: <prompt>\`, \`${normalizedName}(profile=name,root=true,ask=true) <prompt>\`, \`${askName} <question>\`, \`${askName}: <question>\`, \`${planName} <prompt>\`, \`${planName}: <prompt>\`, or \`${planName}(profile=name) <prompt>\`.`;
}
