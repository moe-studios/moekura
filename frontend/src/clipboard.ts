// Buttons that copy a bit of text (`data-copy-text`), such as an upload's
// file number. They're hidden until this shows them.

import { t } from "./i18n.ts";
import { toast } from "./toast.ts";

export function enableClipboard(root: Document = document): void {
  const buttons = root.querySelectorAll<HTMLButtonElement>("button[data-copy-text]");
  if (buttons.length === 0 || !navigator.clipboard) return;
  for (const button of buttons) {
    button.hidden = false;
    button.addEventListener("click", () => {
      const text = button.dataset["copyText"] ?? "";
      navigator.clipboard.writeText(text).then(
        () => toast(t("copied", "Copied {$text}.", { text })),
        () => toast(t("copy-failed", "Couldn't copy it.")),
      );
    });
  }
}
