// Small layout helpers: opening the post's edit form from its link, and
// the footer's shortcuts button.

import { showHelp } from "./keyboard.ts";

/** Opens the <details> around `target`, then focuses it. */
function reveal(target: Element | null): void {
  if (!(target instanceof HTMLElement)) return;
  const details = target.closest("details");
  if (details) details.open = true;
  target.focus();
}

export function enableLayout(): void {
  for (const link of document.querySelectorAll<HTMLAnchorElement>("a[data-open-edit]")) {
    link.addEventListener("click", (event) => {
      event.preventDefault();
      reveal(document.querySelector(link.hash));
    });
  }
  // Also when arriving with the fragment in the address.
  if (window.location.hash === "#edit-tags") reveal(document.getElementById("edit-tags"));

  const shortcuts = document.querySelector<HTMLElement>("[data-shortcuts-link]");
  if (shortcuts) {
    shortcuts.hidden = false;
    shortcuts.querySelector("button")?.addEventListener("click", showHelp);
  }
}
