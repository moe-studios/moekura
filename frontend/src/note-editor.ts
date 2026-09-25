// The note editor on post pages, for those allowed to edit notes. "Edit
// notes" turns it on: drag on the image to draw a note, drag a note to
// move it, drag its bottom-right corner to resize it, and click a note to
// change its text or delete it. Each change is saved through the API and
// the page reloads to show it. Without scripts, notes can't be drawn, but
// their history can still be reverted.

export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface Point {
  x: number;
  y: number;
}

/** The box between two corners, kept inside a `width` × `height` image. */
export function boxBetween(a: Point, b: Point, width: number, height: number): Box {
  const clamp = (v: number, max: number) => Math.min(Math.max(Math.round(v), 0), max);
  const [x1, x2] = [clamp(Math.min(a.x, b.x), width), clamp(Math.max(a.x, b.x), width)];
  const [y1, y2] = [clamp(Math.min(a.y, b.y), height), clamp(Math.max(a.y, b.y), height)];
  return { x: x1, y: y1, width: x2 - x1, height: y2 - y1 };
}

/** `box` moved by `dx`, `dy`, kept inside the image. */
export function moved(box: Box, dx: number, dy: number, width: number, height: number): Box {
  const x = Math.min(Math.max(Math.round(box.x + dx), 0), Math.max(width - box.width, 0));
  const y = Math.min(Math.max(Math.round(box.y + dy), 0), Math.max(height - box.height, 0));
  return { ...box, x, y };
}

/** `box` with its bottom-right corner moved by `dx`, `dy`; at least 1×1. */
export function resized(box: Box, dx: number, dy: number, width: number, height: number): Box {
  const w = Math.min(Math.max(Math.round(box.width + dx), 1), width - box.x);
  const h = Math.min(Math.max(Math.round(box.height + dy), 1), height - box.y);
  return { ...box, width: w, height: h };
}

/** Smallest box, in screen pixels, that counts as drawn rather than a click. */
const MIN_DRAG = 4;
/** How close to a note's corner, in screen pixels, a drag resizes it. */
const HANDLE = 12;

async function send(method: string, url: string, body?: unknown): Promise<string | null> {
  const init: RequestInit = {
    method,
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    credentials: "same-origin",
  };
  if (body !== undefined) init.body = JSON.stringify(body);
  const response = await fetch(url, init);
  if (response.ok) return null;
  try {
    const error = (await response.json()) as { error?: { message?: string } };
    return error.error?.message ?? `Error ${response.status}`;
  } catch {
    return `Error ${response.status}`;
  }
}

export function enableNoteEditor(root: Document = document): void {
  const layer = root.querySelector<HTMLElement>("[data-notes-editable]");
  const svg = layer?.querySelector<SVGSVGElement>("svg.notes");
  const post = layer?.dataset["post"];
  if (!layer || !svg || !post) return;
  const [, , width, height] = (svg.getAttribute("viewBox") ?? "0 0 1 1").split(" ").map(Number) as [
    number,
    number,
    number,
    number,
  ];

  // Image pixels for a pointer event.
  const toImage = (event: PointerEvent): Point => {
    const box = svg.getBoundingClientRect();
    return {
      x: ((event.clientX - box.left) / box.width) * width,
      y: ((event.clientY - box.top) / box.height) * height,
    };
  };
  const scale = () => svg.getBoundingClientRect().width / width;

  const toggle = root.createElement("button");
  toggle.type = "button";
  toggle.className = "secondary note-toggle";
  toggle.textContent = "Edit notes";
  toggle.setAttribute("aria-pressed", "false");
  (root.querySelector("[data-notes-toggle]") ?? layer).after(toggle);
  toggle.addEventListener("click", () => {
    const on = !layer.classList.contains("editing-notes");
    layer.classList.toggle("editing-notes", on);
    layer.classList.remove("notes-hidden");
    toggle.setAttribute("aria-pressed", String(on));
    toggle.textContent = on ? "Done editing notes" : "Edit notes";
    if (!on) closeForm();
  });

  // The form for a note's text.
  const form = root.createElement("form");
  form.className = "note-form";
  form.hidden = true;
  form.innerHTML = `
    <label for="note-body">Note</label>
    <textarea id="note-body" rows="4" maxlength="10000" required></textarea>
    <p class="form-error" role="alert" hidden></p>
    <div class="actions">
      <button type="submit">Save</button>
      <button type="button" class="secondary" data-cancel>Cancel</button>
      <button type="button" class="danger" data-delete>Delete</button>
    </div>`;
  toggle.after(form);
  const text = form.querySelector("textarea")!;
  const error = form.querySelector<HTMLElement>(".form-error")!;
  const deleteButton = form.querySelector<HTMLButtonElement>("[data-delete]")!;
  interface Editing {
    id?: string | undefined;
    version?: string | undefined;
    body?: string | undefined;
    box: Box;
  }
  let editing: Editing | null = null;
  let draft: SVGRectElement | null = null;

  function closeForm(): void {
    form.hidden = true;
    editing = null;
    draft?.remove();
    draft = null;
  }
  const openForm = (target: Editing) => {
    editing = target;
    text.value = target.body ?? "";
    error.hidden = true;
    deleteButton.hidden = target.id === undefined;
    form.hidden = false;
    text.focus();
  };
  const done = (message: string | null) => {
    if (message === null) {
      window.location.reload();
    } else {
      error.textContent = message;
      error.hidden = false;
    }
  };

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!editing) return;
    const body = text.value;
    if (editing.id === undefined) {
      void send("POST", `/api/v1/posts/${post}/notes`, { ...editing.box, body }).then(done);
    } else {
      void send("PUT", `/api/v1/notes/${editing.id}`, { body, base_version: Number(editing.version) }).then(done);
    }
  });
  form.querySelector("[data-cancel]")!.addEventListener("click", closeForm);
  deleteButton.addEventListener("click", () => {
    if (!editing?.id) return;
    void send("DELETE", `/api/v1/notes/${editing.id}?base_version=${editing.version}`).then(done);
  });

  const boxOf = (rect: SVGRectElement): Box => ({
    x: Number(rect.getAttribute("x")),
    y: Number(rect.getAttribute("y")),
    width: Number(rect.getAttribute("width")),
    height: Number(rect.getAttribute("height")),
  });
  const draw = (rect: SVGRectElement, box: Box) => {
    rect.setAttribute("x", String(box.x));
    rect.setAttribute("y", String(box.y));
    rect.setAttribute("width", String(box.width));
    rect.setAttribute("height", String(box.height));
  };

  svg.addEventListener("pointerdown", (event) => {
    if (!layer.classList.contains("editing-notes") || event.button !== 0) return;
    event.preventDefault();
    svg.setPointerCapture(event.pointerId);
    const start = toImage(event);
    const target = (event.target as Element).closest<SVGRectElement>("rect.note-box");
    let mode: "draw" | "move" | "resize";
    let original: Box;
    let rect: SVGRectElement;
    if (target && !target.classList.contains("draft")) {
      rect = target;
      original = boxOf(rect);
      const corner = { x: original.x + original.width, y: original.y + original.height };
      const near = Math.hypot(corner.x - start.x, corner.y - start.y) * scale() < HANDLE;
      mode = near ? "resize" : "move";
    } else {
      closeForm();
      mode = "draw";
      original = { x: start.x, y: start.y, width: 0, height: 0 };
      rect = root.createElementNS("http://www.w3.org/2000/svg", "rect");
      rect.setAttribute("class", "note-box draft");
      rect.setAttribute("vector-effect", "non-scaling-stroke");
      svg.append(rect);
      draft = rect;
    }
    let current = original;
    let dragged = false;

    const onMove = (move: PointerEvent) => {
      const at = toImage(move);
      const [dx, dy] = [at.x - start.x, at.y - start.y];
      if (Math.hypot(dx, dy) * scale() >= MIN_DRAG) dragged = true;
      if (!dragged) return;
      current =
        mode === "draw"
          ? boxBetween(start, at, width, height)
          : mode === "move"
            ? moved(original, dx, dy, width, height)
            : resized(original, dx, dy, width, height);
      draw(rect, current);
    };
    const onUp = () => {
      svg.removeEventListener("pointermove", onMove);
      svg.removeEventListener("pointerup", onUp);
      svg.removeEventListener("pointercancel", onUp);
      if (mode === "draw") {
        if (dragged && current.width > 0 && current.height > 0) openForm({ box: current });
        else closeForm();
        return;
      }
      const id = rect.dataset["note"];
      const version = rect.dataset["version"];
      if (!dragged) {
        openForm({ id, version, body: rect.dataset["body"], box: original });
        return;
      }
      void send("PUT", `/api/v1/notes/${id}`, { ...current, base_version: Number(version) }).then((message) => {
        if (message === null) {
          window.location.reload();
        } else {
          draw(rect, original);
          window.alert(message);
        }
      });
    };
    svg.addEventListener("pointermove", onMove);
    svg.addEventListener("pointerup", onUp);
    svg.addEventListener("pointercancel", onUp);
  });
}
