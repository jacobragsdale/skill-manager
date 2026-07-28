import React from "react";
import ReactDOM from "react-dom/client";
import { Theme } from "@radix-ui/themes";

/*
  `@radix-ui/themes/styles.css` carries all thirty-one Radix colour scales.
  Skill Manager renders five of them — the blue accent, the slate gray, and
  amber, green, and red for status — so the rest is a hundred kilobytes of
  custom properties the webview parses on every launch and never reads. The
  token files are imported individually instead. `tokens/base.css` maps
  `--accent-*` and `--gray-*` onto whichever scale a `color` prop names, so a
  scale is needed here only if some element actually asks for it.

  These imports come before `App`, whose own stylesheet overrides them.
*/
import "@radix-ui/themes/tokens/base.css";
import "@radix-ui/themes/tokens/colors/blue.css";
import "@radix-ui/themes/tokens/colors/slate.css";
import "@radix-ui/themes/tokens/colors/gray.css";
import "@radix-ui/themes/tokens/colors/amber.css";
import "@radix-ui/themes/tokens/colors/green.css";
import "@radix-ui/themes/tokens/colors/red.css";
import "@radix-ui/themes/components.css";
import "@radix-ui/themes/utilities.css";

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
      <App />
    </Theme>
  </React.StrictMode>
);
