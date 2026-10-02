// The post form of an uploaded file. Ctrl+Enter (⌘+Enter) posts it, the
// tags box counts its tags as they're typed, and the form can sit to the
// right of the file, to its left or below it (its buttons, or Shift+R,
// Shift+L and Shift+B), as wide as the divider is dragged; where and how
// wide are remembered.

import { t } from "./i18n.ts";
import { EDIT_METATAGS } from "./metatags.ts";

export type Dock = "right" | "left" | "bottom";

const DOCK_KEY = "upload-dock";
const WIDTH_KEY = "upload-form-width";
const MIN_WIDTH = 280;
/** Room the file keeps beside the form. */
const MIN_MEDIA = 240;
const STEP = 24;

/** How many tags `text` (a tags box) adds: words that aren't removals
 * (`-tag`) or metatags (`rating:s`). Pure, for tests. */
export function tagCount(text: string): number {
  return text
    .split(/\s+/)
    .filter((word) => {
      if (word === "" || word.startsWith("-")) return false;
      const colon = word.indexOf(":");
      return colon <= 0 || !(word.slice(0, colon).toLowerCase() in EDIT_METATAGS);
    }).length;
}

/** `value` as a dock position, or null. */
export function asDock(value: string | null | undefined): Dock | null {
  return value === "right" || value === "left" || value === "bottom" ? value : null;
}

function stored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // Private browsing: it's just not remembered.
  }
}

export function enableUploadForm(root: Document = document): void {
  const form = root.querySelector<HTMLFormElement>("form#upload-form");
  if (!form) return;

  form.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      form.querySelector<HTMLButtonElement>("button[data-upload-post]")?.click();
    }
  });
  const hint = form.querySelector<HTMLElement>("[data-ctrl-enter-hint]");
  if (hint) hint.hidden = false;

  const tags = form.querySelector<HTMLTextAreaElement>("textarea[name=tags]");
  const counter = form.querySelector<HTMLElement>("[data-tag-count]");
  if (tags && counter) {
    const update = () => {
      const count = tagCount(tags.value);
      counter.textContent = t("tag-count", "Tags: {$count}", { count });
    };
    tags.addEventListener("input", update);
    counter.hidden = false;
    update();
  }

  const layout = root.querySelector<HTMLElement>("[data-upload-layout]");
  const divider = layout?.querySelector<HTMLElement>("[data-upload-divider]");
  if (!layout || !divider) return;
  const setDock = (dock: Dock) => {
    layout.dataset["dock"] = dock;
    for (const button of root.querySelectorAll<HTMLButtonElement>("button[data-dock-to]")) {
      button.setAttribute("aria-pressed", String(button.dataset["dockTo"] === dock));
    }
  };
  const setWidth = (width: number) => {
    const most = Math.max(MIN_WIDTH, layout.clientWidth - MIN_MEDIA);
    const clamped = Math.round(Math.min(Math.max(width, MIN_WIDTH), most));
    layout.style.setProperty("--upload-form-width", `${clamped}px`);
    divider.setAttribute("aria-valuenow", String(clamped));
    return clamped;
  };
  setDock(asDock(stored(DOCK_KEY)) ?? "right");
  const width = Number(stored(WIDTH_KEY));
  if (width > 0) setWidth(width);

  for (const button of root.querySelectorAll<HTMLButtonElement>("button[data-dock-to]")) {
    button.addEventListener("click", () => {
      const dock = asDock(button.dataset["dockTo"]);
      if (!dock) return;
      setDock(dock);
      store(DOCK_KEY, dock);
    });
  }
  const docks = root.querySelector<HTMLElement>("[data-upload-docks]");
  if (docks) docks.hidden = false;

  // The form's width, from where the pointer is.
  const widthAt = (x: number): number => {
    const box = layout.getBoundingClientRect();
    return layout.dataset["dock"] === "left" ? x - box.left : box.right - x;
  };
  divider.addEventListener("pointerdown", (event) => {
    if (layout.dataset["dock"] === "bottom") return;
    event.preventDefault();
    divider.setPointerCapture(event.pointerId);
    layout.classList.add("resizing");
    const move = (e: PointerEvent) => setWidth(widthAt(e.clientX));
    const done = (e: PointerEvent) => {
      store(WIDTH_KEY, String(setWidth(widthAt(e.clientX))));
      layout.classList.remove("resizing");
      divider.removeEventListener("pointermove", move);
      divider.removeEventListener("pointerup", done);
      divider.removeEventListener("pointercancel", done);
    };
    divider.addEventListener("pointermove", move);
    divider.addEventListener("pointerup", done);
    divider.addEventListener("pointercancel", done);
  });
  divider.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const current = Number(divider.getAttribute("aria-valuenow")) || form.getBoundingClientRect().width;
    const wider = (event.key === "ArrowLeft") === (layout.dataset["dock"] !== "left");
    store(WIDTH_KEY, String(setWidth(current + (wider ? STEP : -STEP))));
  });
}
