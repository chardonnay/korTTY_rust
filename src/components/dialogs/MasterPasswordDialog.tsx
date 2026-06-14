import { CSSProperties, FormEvent, useEffect, useRef, useState } from "react";
import korttyLogo from "../../assets/kortty_logo.png";
import korttyLogoVideo from "../../assets/kortty_logo_ai_pingpong.mp4";

interface MasterPasswordDialogProps {
  mode: "setup" | "unlock";
  busy: boolean;
  error: string | null;
  onSubmit: (password: string) => Promise<void> | void;
}

/**
 * Master password screen, ported from the Java v2.2 login refresh:
 * hardcoded black background (independent of the active app design),
 * large KorTTY logo, light text and a blue password field with dark
 * input text (see Java MasterPasswordDialog.stylePasswordField()).
 */

const PASSWORD_FIELD_STYLE: CSSProperties = {
  backgroundColor: "#2f9cff",
  color: "#000000",
  border: "1.5px solid #79c4ff",
  borderRadius: "6px",
  caretColor: "#000000",
};

const PRIMARY_BUTTON_STYLE: CSSProperties = {
  backgroundColor: "#16324a",
  border: "1px solid #2f9cff",
  borderRadius: "6px",
  color: "#ffffff",
};

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
    <div
      className="min-h-screen flex items-center justify-center px-8"
      style={{ backgroundColor: "#000000", color: "#f1f5fb" }}
    >
      <div className="flex w-full max-w-5xl items-center justify-center gap-12">
        <form onSubmit={handleSubmit} className="w-full max-w-sm space-y-5">
          <div>
            <h1 className="text-lg font-bold" style={{ color: "#f1f5fb" }}>
              {title}
            </h1>
            <p className="mt-2 text-sm leading-6" style={{ color: "#d8e3f0" }}>
              {description}
            </p>
          </div>

          <div>
            <label
              className="block text-[11px] uppercase tracking-wide font-bold mb-1.5"
              style={{ color: "#f1f5fb" }}
            >
              {isSetup ? "New Master Password" : "Master Password"}
            </label>
            <input
              ref={passwordRef}
              className="w-full px-3 py-2 text-sm focus:outline-none placeholder-black/60"
              style={PASSWORD_FIELD_STYLE}
              type="password"
              autoComplete={isSetup ? "new-password" : "current-password"}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              disabled={busy}
            />
          </div>

          {isSetup && (
            <div>
              <label
                className="block text-[11px] uppercase tracking-wide font-bold mb-1.5"
                style={{ color: "#f1f5fb" }}
              >
                Confirm Password
              </label>
              <input
                className="w-full px-3 py-2 text-sm focus:outline-none placeholder-black/60"
                style={PASSWORD_FIELD_STYLE}
                type="password"
                autoComplete="new-password"
                value={confirmPassword}
                onChange={(event) => setConfirmPassword(event.target.value)}
                disabled={busy}
              />
              <p className="mt-2 text-xs" style={{ color: "#d8e3f0" }}>
                Use at least 8 characters. KorTTY will ask for this password on every startup.
              </p>
            </div>
          )}

          {displayedError && (
            <div
              className="rounded-md px-3 py-2 text-xs font-bold"
              style={{
                color: "#ff6b6b",
                border: "1px solid rgba(255,107,107,0.4)",
                backgroundColor: "rgba(255,107,107,0.08)",
              }}
            >
              {displayedError}
            </div>
          )}

          <button
            type="submit"
            disabled={busy}
            className="w-full px-3 py-2 text-sm font-bold cursor-pointer hover:opacity-90 disabled:opacity-60 disabled:cursor-not-allowed transition-opacity"
            style={PRIMARY_BUTTON_STYLE}
          >
            {busy ? "Please wait..." : submitLabel}
          </button>
        </form>

        <div className="hidden md:flex flex-col items-center justify-center select-none">
          <video
            autoPlay
            loop
            muted
            playsInline
            draggable={false}
            style={{
              width: isSetup ? 220 : 420,
              maxWidth: "40vw",
              height: "auto",
            }}
            onError={(e) => {
              const video = e.currentTarget;
              const fallback = video.nextElementSibling as HTMLImageElement | null;
              if (fallback) fallback.style.display = "block";
              video.style.display = "none";
            }}
          >
            <source src={korttyLogoVideo} type="video/mp4" />
          </video>
          <img
            src={korttyLogo}
            alt="KorTTY"
            draggable={false}
            style={{
              display: "none",
              width: isSetup ? 220 : 420,
              maxWidth: "40vw",
              height: "auto",
            }}
          />
        </div>
      </div>
    </div>
  );
}
