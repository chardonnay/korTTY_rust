import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MainWindow } from "./components/MainWindow";
import { MasterPasswordDialog } from "./components/dialogs/MasterPasswordDialog";

type AuthPhase = "checking" | "setup" | "unlock" | "ready";

type MasterPasswordStatus = {
  hasPassword: boolean;
  unlocked: boolean;
};

export default function App() {
  const [authPhase, setAuthPhase] = useState<AuthPhase>("checking");
  const [authError, setAuthError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const loadAuthStatus = useCallback(async () => {
    const status = await invoke<MasterPasswordStatus>("get_master_password_status");
    if (status.unlocked) {
      setAuthPhase("ready");
      return;
    }

    setAuthPhase(status.hasPassword ? "unlock" : "setup");
  }, []);

  useEffect(() => {
    loadAuthStatus().catch((error) => {
      setAuthError(String(error));
      setAuthPhase("unlock");
    });
  }, [loadAuthStatus]);

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
