import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { RootErrorBoundary } from "./components/common/RootErrorBoundary";
import "./i18n";
import "./styles/globals.css";
import "./styles/terminal.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <RootErrorBoundary>
      <App />
    </RootErrorBoundary>
  </React.StrictMode>,
);
