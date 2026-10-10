// Keyboard shortcuts. Each acts on a link or control the page already
// has, so the page decides what "next" means (the next post in a search,
// or the next page of results).

import { t } from "./i18n.ts";

/** Keys, the message naming what they do, and its English text. */
export const SHORTCUTS: readonly (readonly [string, string, string])[] = [
  ["a, ←", "previous", "Previous post or page"],
  ["d, →", "next", "Next post or page"],
  ["e", "edit", "Edit the post"],
  ["f", "favorite", "Favorite the post"],
  ["n", "notes", "Show or hide notes"],
  ["/", "search", "Search"],
  ["Shift+R, Shift+L, Shift+B", "dock", "Upload form to the right, left or bottom"],
  ["?", "help", "Show these shortcuts"],
];

/** The action for a key, or null. Pure, for tests. */
export function actionFor(key: string): string | null {
  switch (key) {
    case "a":
    case "ArrowLeft":
      return "prev";
    case "d":
    case "ArrowRight":
      return "next";
    case "e":
      return "edit";
    case "f":
      return "favorite";
    case "n":
      return "notes";
    case "/":
      return "search";
    case "?":
      return "help";
    case "R":
      return "dock-right";
    case "L":
      return "dock-left";
    case "B":
      return "dock-bottom";
    default:
      return null;
  }
}

/** Whether typing in (or otherwise using keys on) `target` should win. */
function busy(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return (
    target.closest("input, textarea, select, button, video, audio, [contenteditable]") !== null ||
    document.querySelector("dialog[open]") !== null
  );
}

export function showHelp(): void {
  const existing = document.getElementById("shortcuts");
  const dialog = existing instanceof HTMLDialogElement ? existing : document.createElement("dialog");
  if (!existing) {
    dialog.id = "shortcuts";
    dialog.className = "shortcuts";
    const title = document.createElement("h2");
    title.textContent = t("shortcuts-title", "Keyboard shortcuts");
    const list = document.createElement("dl");
    for (const [keys, key, english] of SHORTCUTS) {
      const dt = document.createElement("dt");
      dt.textContent = keys;
      const dd = document.createElement("dd");
      dd.textContent = t(`shortcut-${key}`, english);
      list.append(dt, dd);
    }
    const close = document.createElement("button");
    close.type = "button";
    close.textContent = t("close", "Close");
    close.addEventListener("click", () => dialog.close());
    dialog.append(title, list, close);
    // A click on the backdrop closes it.
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) dialog.close();
    });
    document.body.append(dialog);
  }
  dialog.showModal();
}

/** Focuses `field` with the caret after its text, past a space, so what
 * is typed next starts a new tag instead of joining the last one. */
export function focusAtEnd(field: HTMLElement): void {
  field.focus();
  if (!(field instanceof HTMLTextAreaElement || field instanceof HTMLInputElement)) return;
  if (field.readOnly || field.disabled) return;
  if (field.value !== "" && !/\s$/.test(field.value)) field.value += " ";
  const end = field.value.length;
  field.setSelectionRange(end, end);
}

function run(action: string): boolean {
  switch (action) {
    case "prev":
    case "next": {
      const link = document.querySelector<HTMLAnchorElement>(`a[rel=${action}]`);
      if (!link) return false;
      window.location.assign(link.href);
      return true;
    }
    case "edit": {
      const details = document.querySelector<HTMLDetailsElement>("details#edit");
      if (!details) return false;
      details.open = true;
      const textarea = details.querySelector("textarea");
      if (textarea) focusAtEnd(textarea);
      return true;
    }
    case "favorite": {
      const button = document.querySelector<HTMLButtonElement>(".favorite button");
      if (!button) return false;
      button.click();
      return true;
    }
    case "notes": {
      const button = document.querySelector<HTMLButtonElement>("[data-notes-toggle]");
      if (!button) return false;
      button.click();
      return true;
    }
    case "search": {
      const input = document.querySelector<HTMLInputElement>("form[role=search] input[name=tags]");
      if (!input) return false;
      input.focus();
      input.select();
      return true;
    }
    case "help":
      showHelp();
      return true;
    case "dock-right":
    case "dock-left":
    case "dock-bottom": {
      const button = document.querySelector<HTMLButtonElement>(`button[data-dock-to=${action.slice(5)}]`);
      if (!button) return false;
      button.click();
      return true;
    }
    default:
      return false;
  }
}

export function enableShortcuts(): void {
  document.addEventListener("keydown", (event) => {
    if (event.ctrlKey || event.metaKey || event.altKey || busy(event.target)) return;
    const action = actionFor(event.key);
    if (action && run(action)) event.preventDefault();
  });
}
