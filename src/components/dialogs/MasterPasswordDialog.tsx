import { FormEvent, useEffect, useRef, useState } from "react";
import { KeyRound, LockKeyhole } from "lucide-react";

interface MasterPasswordDialogProps {
  mode: "setup" | "unlock";
  busy: boolean;
  error: string | null;
  onSubmit: (password: string) => Promise<void> | void;
}

export function MasterPasswordDialog({
  mode,
  busy,
  error,
  onSubmit,
}: MasterPasswordDialogProps) {
  const passwordRef = useRef<HTMLInputElement | null>(null);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    setPassword("");
    setConfirmPassword("");
    setLocalError(null);
  }, [mode]);

  useEffect(() => {
    passwordRef.current?.focus();
  }, [mode, busy]);

  const isSetup = mode === "setup";
  const title = isSetup ? "Create Master Password" : "Unlock KorTTY";
  const description = isSetup
    ? "KorTTY needs a master password before the application can be opened. This password will be required every time the app starts."
    : "Please enter your master password to continue. KorTTY requires this step on every application start.";
  const submitLabel = isSetup ? "Set Password" : "Unlock";
  const displayedError = localError ?? error;

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setLocalError(null);

    if (!password) {
      setLocalError(isSetup ? "Please enter a new master password." : "Please enter your master password.");
      return;
    }

    if (isSetup && password !== confirmPassword) {
      setLocalError("The confirmation password does not match.");
      return;
    }

    await onSubmit(password);
  }

  return (
    <div className="min-h-screen bg-kortty-bg text-kortty-text flex items-center justify-center px-4">
      <div className="w-full max-w-md bg-kortty-surface border border-kortty-border rounded-xl shadow-2xl overflow-hidden">
        <div className="px-5 py-4 border-b border-kortty-border flex items-center gap-3">
          <div className="w-10 h-10 rounded-full bg-kortty-accent/10 text-kortty-accent flex items-center justify-center">
            {isSetup ? <KeyRound className="w-5 h-5" /> : <LockKeyhole className="w-5 h-5" />}
          </div>
          <div>
            <h1 className="text-sm font-semibold">{title}</h1>
            <p className="text-xs text-kortty-text-dim">Secure startup required</p>
          </div>
        </div>

        <form onSubmit={handleSubmit} className="px-5 py-4 space-y-4">
          <p className="text-sm text-kortty-text-dim leading-6">{description}</p>

          <div>
            <label className="block text-[11px] uppercase tracking-wide text-kortty-text-dim mb-1.5">
              {isSetup ? "New Master Password" : "Master Password"}
            </label>
            <input
              ref={passwordRef}
              className="input-field"
              type="password"
              autoComplete={isSetup ? "new-password" : "current-password"}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              disabled={busy}
            />
          </div>

          {isSetup && (
            <div>
              <label className="block text-[11px] uppercase tracking-wide text-kortty-text-dim mb-1.5">
                Confirm Password
              </label>
              <input
                className="input-field"
                type="password"
                autoComplete="new-password"
                value={confirmPassword}
                onChange={(event) => setConfirmPassword(event.target.value)}
                disabled={busy}
              />
              <p className="mt-2 text-xs text-kortty-text-dim">
                Use at least 8 characters. KorTTY will ask for this password on every startup.
              </p>
            </div>
          )}

          {displayedError && (
            <div className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-xs text-red-200">
              {displayedError}
            </div>
          )}

          <button
            type="submit"
            disabled={busy}
            className="w-full px-3 py-2 rounded-md bg-kortty-accent text-black text-sm font-medium hover:opacity-90 disabled:opacity-60 disabled:cursor-not-allowed transition-opacity"
          >
            {busy ? "Please wait..." : submitLabel}
          </button>
        </form>
      </div>
    </div>
  );
}
