// Keyboard shortcuts. Each acts on a link or control the page already
// has, so the page decides what "next" means (the next post in a search,
// or the next page of results).

export const SHORTCUTS: readonly (readonly [string, string])[] = [
  ["a, ←", "Previous post or page"],
  ["d, →", "Next post or page"],
  ["e", "Edit the post"],
  ["f", "Favorite the post"],
  ["n", "Show or hide notes"],
  ["/", "Search"],
  ["?", "Show these shortcuts"],
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

function showHelp(): void {
  const existing = document.getElementById("shortcuts");
  const dialog = existing instanceof HTMLDialogElement ? existing : document.createElement("dialog");
  if (!existing) {
    dialog.id = "shortcuts";
    dialog.className = "shortcuts";
    const title = document.createElement("h2");
    title.textContent = "Keyboard shortcuts";
    const list = document.createElement("dl");
    for (const [keys, what] of SHORTCUTS) {
      const dt = document.createElement("dt");
      dt.textContent = keys;
      const dd = document.createElement("dd");
      dd.textContent = what;
      list.append(dt, dd);
    }
    const close = document.createElement("button");
    close.type = "button";
    close.textContent = "Close";
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
      details.querySelector("textarea")?.focus();
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
      const input = document.querySelector<HTMLInputElement>(".site-header input[name=tags]");
      if (!input) return false;
      input.focus();
      input.select();
      return true;
    }
    case "help":
      showHelp();
      return true;
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
