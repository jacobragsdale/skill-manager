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
    <Theme appearance="dark" accentColor="grass" grayColor="sage" panelBackground="translucent" radius="large" scaling="100%">
      <MotionConfig reducedMotion="user">
        <App />
      </MotionConfig>
    </Theme>
  </React.StrictMode>
);
