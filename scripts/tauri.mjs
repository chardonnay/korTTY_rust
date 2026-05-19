#!/usr/bin/env node

import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const command = args[0];
const shouldUseCiMode =
  process.platform === "darwin" &&
  (command === "build" || command === "bundle") &&
  !args.includes("--ci") &&
  process.env.TAURI_BUNDLER_DMG_IGNORE_CI !== "1";

const tauriArgs = shouldUseCiMode
  ? [command, "--ci", ...args.slice(1)]
  : args;
const repoRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const tauriBin = join(
  repoRoot,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.cmd" : "tauri",
);

const child = spawn(tauriBin, tauriArgs, {
  stdio: "inherit",
  shell: process.platform === "win32",
});

child.on("error", (error) => {
  console.error(`Failed to run Tauri CLI: ${error.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }

  process.exit(code ?? 1);
});
