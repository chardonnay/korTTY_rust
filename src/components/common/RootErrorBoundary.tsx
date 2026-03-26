import React from "react";

type RootErrorBoundaryProps = {
  children: React.ReactNode;
};

type RootErrorBoundaryState = {
  errorMessage: string | null;
  errorStack: string | null;
  source: "render" | "runtime" | null;
};

function formatUnknownError(error: unknown): { message: string; stack: string | null } {
  if (error instanceof Error) {
    return {
      message: error.message || error.name || "Unknown frontend error.",
      stack: error.stack || null,
    };
  }

  const message = String(error || "Unknown frontend error.");
  return { message, stack: null };
}

export class RootErrorBoundary extends React.Component<
  RootErrorBoundaryProps,
  RootErrorBoundaryState
> {
  state: RootErrorBoundaryState = {
    errorMessage: null,
    errorStack: null,
    source: null,
  };

  static getDerivedStateFromError(error: Error): RootErrorBoundaryState {
    const formatted = formatUnknownError(error);
    return {
      errorMessage: formatted.message,
      errorStack: formatted.stack,
      source: "render",
    };
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("RootErrorBoundary caught a render error.", error, errorInfo);
    this.setState((current) => ({
      errorMessage: current.errorMessage || error.message || "Unknown frontend error.",
      errorStack: current.errorStack || error.stack || errorInfo.componentStack || null,
      source: "render",
    }));
  }

  componentDidMount() {
    window.addEventListener("error", this.handleWindowError);
    window.addEventListener("unhandledrejection", this.handleUnhandledRejection);
  }

  componentWillUnmount() {
    window.removeEventListener("error", this.handleWindowError);
    window.removeEventListener("unhandledrejection", this.handleUnhandledRejection);
  }

  handleWindowError = (event: ErrorEvent) => {
    const formatted = formatUnknownError(event.error ?? event.message);
    console.error("Unhandled window error.", event.error ?? event.message);
    this.setState({
      errorMessage: formatted.message,
      errorStack: formatted.stack,
      source: "runtime",
    });
  };

  handleUnhandledRejection = (event: PromiseRejectionEvent) => {
    const formatted = formatUnknownError(event.reason);
    console.error("Unhandled promise rejection.", event.reason);
    this.setState({
      errorMessage: formatted.message,
      errorStack: formatted.stack,
      source: "runtime",
    });
  };

  handleReload = () => {
    window.location.reload();
  };

  render() {
    if (!this.state.errorMessage) {
      return this.props.children;
    }

    return (
      <div className="min-h-screen bg-kortty-bg text-kortty-text flex items-center justify-center p-6">
        <div className="w-full max-w-3xl rounded-xl border border-kortty-border bg-kortty-surface shadow-2xl overflow-hidden">
          <div className="border-b border-kortty-border px-5 py-4">
            <div className="text-sm font-semibold">Frontend Error</div>
            <div className="mt-1 text-xs text-kortty-text-dim">
              KorTTY has caught a {this.state.source === "render" ? "render" : "runtime"} error instead of leaving the window blank.
            </div>
          </div>
          <div className="space-y-4 px-5 py-4">
            <div className="rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-200">
              {this.state.errorMessage}
            </div>
            {this.state.errorStack && (
              <pre className="max-h-80 overflow-auto rounded-md border border-kortty-border bg-kortty-terminal px-3 py-3 text-xs text-kortty-text whitespace-pre-wrap break-words">
                {this.state.errorStack}
              </pre>
            )}
            <div className="flex justify-end">
              <button
                type="button"
                onClick={this.handleReload}
                className="rounded-md bg-kortty-accent px-4 py-2 text-sm font-medium text-black transition-opacity hover:opacity-90"
              >
                Reload UI
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }
}
