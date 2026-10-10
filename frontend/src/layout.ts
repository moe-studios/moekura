// Small layout helpers: opening the post's edit form from its link, the
// footer's shortcuts button, and closing menus.

import { focusAtEnd, showHelp } from "./keyboard.ts";

/** Opens the <details> around `target`, then focuses it. */
function reveal(target: Element | null): void {
  if (!(target instanceof HTMLElement)) return;
  const details = target.closest("details");
  if (details) details.open = true;
  focusAtEnd(target);
}

/** Menus made of <details data-menu> open without scripts; with them they
 * also close on Escape, on a click elsewhere, and when another opens. */
function enableMenus(): void {
  const menus = () => document.querySelectorAll<HTMLDetailsElement>("details[data-menu][open]");
  document.addEventListener("click", (event) => {
    for (const menu of menus()) {
      if (!menu.contains(event.target as Node)) menu.open = false;
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    for (const menu of menus()) {
      menu.open = false;
      menu.querySelector("summary")?.focus();
    }
  });
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

  enableMenus();

  const shortcuts = document.querySelector<HTMLElement>("[data-shortcuts-link]");
  if (shortcuts) {
    shortcuts.hidden = false;
    shortcuts.querySelector("button")?.addEventListener("click", showHelp);
  }
}
