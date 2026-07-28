import React from "react";
import ReactDOM from "react-dom/client";
import { Theme } from "@radix-ui/themes";
import { MotionConfig } from "motion/react";
import "@radix-ui/themes/styles.css";
import App from "./App";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("The application root element is missing.");
}

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    {/*
      `panelBackground="translucent"` gives every panel a `backdrop-filter:
      blur(64px)`. Windows machines without GPU-accelerated compositing — a VM,
      a remote session, a locked-down work laptop — fall back to software
      blurring and repaint each card on every frame. The cards already declare
      their own near-opaque backgrounds, so a solid panel looks the same and
      costs nothing.
    */}
    <Theme appearance="dark" accentColor="blue" grayColor="slate" panelBackground="solid" radius="medium" scaling="100%">
      <MotionConfig reducedMotion="user">
        <App />
      </MotionConfig>
    </Theme>
  </React.StrictMode>
);
