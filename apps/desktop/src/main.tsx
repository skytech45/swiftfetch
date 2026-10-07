import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { installEventBridge } from "./jobs";
import "./styles.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("missing #root element");
}

installEventBridge();

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
