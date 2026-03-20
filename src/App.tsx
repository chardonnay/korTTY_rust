import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MainWindow } from "./components/MainWindow";
import { MasterPasswordDialog } from "./components/dialogs/MasterPasswordDialog";

type AuthPhase = "checking" | "setup" | "unlock" | "error" | "ready";

type MasterPasswordStatus = {
  hasPassword: boolean;
  unlocked: boolean;
};

export default function App() {
  const [authPhase, setAuthPhase] = useState<AuthPhase>("checking");
  const [authError, setAuthError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const isAuthRequiredError = useCallback((error: unknown) => {
    const message = String(error).toLowerCase();
    return (
      message.includes("authentication required") ||
      message.includes("auth required") ||
      message.includes("master password required")
    );
  }, []);

  const loadAuthStatus = useCallback(async () => {
    setAuthError(null);
    const status = await invoke<MasterPasswordStatus>("get_master_password_status");
    if (status.unlocked) {
      setAuthPhase("ready");
      return;
    }

    setAuthPhase(status.hasPassword ? "unlock" : "setup");
  }, []);

  const retryAuthStatus = useCallback(() => {
    setAuthPhase("checking");
    void loadAuthStatus().catch((error) => {
      const message = String(error);
      setAuthError(message);
      setAuthPhase(isAuthRequiredError(error) ? "unlock" : "error");
    });
  }, [isAuthRequiredError, loadAuthStatus]);

  useEffect(() => {
    loadAuthStatus().catch((error) => {
      const message = String(error);
      setAuthError(message);
      setAuthPhase(isAuthRequiredError(error) ? "unlock" : "error");
    });
  }, [isAuthRequiredError, loadAuthStatus]);

  const handleSubmit = useCallback(
    async (password: string) => {
      setSubmitting(true);
      setAuthError(null);

      try {
        if (authPhase === "setup") {
          await invoke("set_master_password", { password });
        } else {
          await invoke("unlock_master_password", { password });
        }
        setAuthPhase("ready");
      } catch (error) {
        setAuthError(String(error));
      } finally {
        setSubmitting(false);
      }
    },
    [authPhase],
  );

  if (authPhase === "checking") {
    return (
      <div className="min-h-screen bg-kortty-bg text-kortty-text flex items-center justify-center">
        <div className="bg-kortty-surface border border-kortty-border rounded-lg px-6 py-5 shadow-2xl text-sm">
          Initializing secure startup...
        </div>
      </div>
    );
  }

  if (authPhase === "error") {
    return (
      <div className="min-h-screen bg-kortty-bg text-kortty-text flex items-center justify-center px-4">
        <div className="w-full max-w-md bg-kortty-surface border border-kortty-border rounded-xl shadow-2xl overflow-hidden">
          <div className="px-5 py-4 border-b border-kortty-border">
            <h1 className="text-sm font-semibold">Secure Startup Error</h1>
            <p className="text-xs text-kortty-text-dim mt-1">
              KorTTY could not read the master password status from the backend.
            </p>
          </div>
          <div className="px-5 py-4 space-y-4">
            <div className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200">
              {authError ?? "Unknown backend error."}
            </div>
            <button
              type="button"
              onClick={retryAuthStatus}
              className="w-full px-3 py-2 rounded-md bg-kortty-accent text-black text-sm font-medium hover:opacity-90 transition-opacity"
            >
              Retry
            </button>
          </div>
        </div>
      </div>
    );
  }

  if (authPhase !== "ready") {
    return (
      <MasterPasswordDialog
        mode={authPhase === "setup" ? "setup" : "unlock"}
        busy={submitting}
        error={authError}
        onSubmit={handleSubmit}
      />
    );
  }

  return <MainWindow />;
}
