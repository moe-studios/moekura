// Shows a note's text beside its box when pointed at, focused or tapped,
// and adds a button to hide the notes, remembered in this browser. Without
// scripts, a box's text is its tooltip and every note is listed below the
// image.

const HIDDEN_KEY = "moekura:notes-hidden";

function remembered(): boolean {
  try {
    return localStorage.getItem(HIDDEN_KEY) === "1";
  } catch {
    return false;
  }
}

function remember(hidden: boolean): void {
  try {
    if (hidden) localStorage.setItem(HIDDEN_KEY, "1");
    else localStorage.removeItem(HIDDEN_KEY);
  } catch {
    // Storage can be off; the choice then lasts for the page.
  }
}

/** Where a popup goes: below the box, kept inside the layer's width. */
export function popupPosition(
  box: { left: number; top: number; width: number; height: number },
  layerWidth: number,
  popupWidth: number,
): { left: number; top: number } {
  const left = Math.max(0, Math.min(box.left, layerWidth - popupWidth));
  return { left, top: box.top + box.height + 4 };
}

export function enableNotes(root: Document = document): void {
  const layer = root.querySelector<HTMLElement>("[data-notes]");
  const svg = layer?.querySelector<SVGSVGElement>("svg.notes");
  if (!layer || !svg) return;

  const popup = root.createElement("div");
  popup.className = "note-popup";
  popup.hidden = true;
  popup.setAttribute("role", "tooltip");
  layer.append(popup);
  let shown: SVGRectElement | null = null;

  const hide = () => {
    popup.hidden = true;
    shown?.classList.remove("active");
    shown = null;
  };
  const show = (rect: SVGRectElement) => {
    const text = root.querySelector(`[data-note-text="${rect.dataset["note"]}"] .markup`);
    if (!text) return;
    shown?.classList.remove("active");
    shown = rect;
    rect.classList.add("active");
    popup.innerHTML = "";
    popup.append(text.cloneNode(true));
    popup.hidden = false;
    const layerBox = layer.getBoundingClientRect();
    const rectBox = rect.getBoundingClientRect();
    const place = popupPosition(
      {
        left: rectBox.left - layerBox.left,
        top: rectBox.top - layerBox.top,
        width: rectBox.width,
        height: rectBox.height,
      },
      layerBox.width,
      popup.offsetWidth,
    );
    popup.style.left = `${place.left}px`;
    popup.style.top = `${place.top}px`;
  };

  for (const rect of svg.querySelectorAll<SVGRectElement>("rect.note-box")) {
    // The popup replaces the tooltip.
    rect.querySelector("title")?.remove();
    rect.setAttribute("tabindex", "0");
    rect.addEventListener("mouseenter", () => show(rect));
    rect.addEventListener("focus", () => show(rect));
    rect.addEventListener("click", (event) => {
      event.preventDefault();
      if (shown === rect) hide();
      else show(rect);
    });
  }
  layer.addEventListener("mouseleave", hide);
  root.addEventListener("keydown", (event) => {
    if (event.key === "Escape") hide();
  });

  const toggle = root.createElement("button");
  toggle.type = "button";
  toggle.className = "secondary note-toggle";
  toggle.dataset["notesToggle"] = "";
  const apply = (hidden: boolean) => {
    layer.classList.toggle("notes-hidden", hidden);
    toggle.textContent = hidden ? "Show notes" : "Hide notes";
    toggle.setAttribute("aria-pressed", String(hidden));
    if (hidden) hide();
  };
  toggle.addEventListener("click", () => {
    const hidden = !layer.classList.contains("notes-hidden");
    remember(hidden);
    apply(hidden);
  });
  apply(remembered());
  layer.after(toggle);
}
