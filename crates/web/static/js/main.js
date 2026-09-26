// Built from frontend/src by npm run build. Do not edit.

// src/metatags.ts
var METATAGS = {
  id: [],
  rating: ["general", "sensitive", "questionable", "explicit"],
  status: ["pending", "active", "flagged", "deleted", "any"],
  user: [],
  score: [],
  favcount: [],
  commentcount: [],
  notecount: [],
  note: [],
  width: [],
  height: [],
  mpixels: [],
  ratio: [],
  filesize: [],
  duration: [],
  date: [],
  filetype: ["jpg", "png", "gif", "webp", "avif", "jxl", "mp4", "webm"],
  md5: [],
  parent: ["none", "any"],
  tagcount: [],
  order: [
    "id",
    "id_asc",
    "score",
    "score_asc",
    "favcount",
    "favcount_asc",
    "mpixels",
    "mpixels_asc",
    "filesize",
    "filesize_asc",
    "landscape",
    "portrait",
    "duration",
    "duration_asc",
    "tagcount",
    "tagcount_asc",
    "random",
    "comment",
    "comment_asc",
    "note",
    "note_asc"
  ],
  limit: [],
  fav: [],
  ordfav: [],
  similar: [],
  pool: ["any", "none"],
  ordpool: [],
  search: ["all"],
  favgroup: [],
  ordfavgroup: [],
  ai: []
};
var CATEGORIES = ["artist", "copyright", "character", "general", "meta"];

// src/autocomplete.ts
var DEBOUNCE_MS = 120;
var cache = /* @__PURE__ */ new Map();
var nextId = 0;
function target(value, caret, mode) {
  let start = caret;
  while (start > 0 && !/\s/.test(value.charAt(start - 1))) start--;
  let end = caret;
  while (end < value.length && !/\s/.test(value.charAt(end))) end++;
  let word = value.slice(start, caret);
  if (mode === "search" && /^[-~]/.test(word)) {
    start += 1;
    word = word.slice(1);
  }
  const colon = word.indexOf(":");
  if (colon > 0) {
    const prefix = word.slice(0, colon).toLowerCase();
    const rest = word.slice(colon + 1);
    if (mode === "search" && prefix in METATAGS) {
      const comma = rest.lastIndexOf(",");
      const typed = rest.slice(comma + 1);
      return { start: start + colon + 1 + comma + 1, end, typed, kind: "metatag-value", metatag: prefix };
    }
    if (CATEGORIES.includes(prefix)) {
      return rest ? { start: start + colon + 1, end, typed: rest, kind: "tag" } : null;
    }
  }
  if (!word || word.includes("*")) return null;
  return { start, end, typed: word, kind: "tag" };
}
async function fetchSuggestions(typed, signal) {
  const key = typed.toLowerCase();
  const cached = cache.get(key);
  if (cached) return cached;
  const response = await fetch(`/tags/autocomplete?q=${encodeURIComponent(typed)}`, {
    signal,
    headers: { Accept: "application/json" }
  });
  if (!response.ok) return [];
  const suggestions = await response.json();
  if (cache.size >= 500) cache.clear();
  cache.set(key, suggestions);
  return suggestions;
}
function metatagNames(typed) {
  const lower = typed.toLowerCase();
  return Object.keys(METATAGS).filter((name) => name.startsWith(lower)).map((name) => ({ text: `${name}:`, finished: false }));
}
function metatagValues(metatag, typed) {
  const lower = typed.toLowerCase();
  return (METATAGS[metatag] ?? []).filter((value) => value.startsWith(lower) && value !== lower).map((value) => ({ text: value, finished: true }));
}
var Autocomplete = class {
  field;
  mode;
  list;
  items = [];
  active = -1;
  current = null;
  timer;
  request;
  constructor(field, mode) {
    this.field = field;
    this.mode = mode;
    const id = `autocomplete-${nextId++}`;
    this.list = document.createElement("ul");
    this.list.id = id;
    this.list.className = "autocomplete";
    this.list.setAttribute("role", "listbox");
    this.list.hidden = true;
    const wrapper = document.createElement("div");
    wrapper.className = "autocomplete-wrap";
    field.replaceWith(wrapper);
    wrapper.append(field, this.list);
    field.setAttribute("role", "combobox");
    field.setAttribute("aria-autocomplete", "list");
    field.setAttribute("aria-controls", id);
    field.setAttribute("aria-expanded", "false");
    field.setAttribute("autocomplete", "off");
    field.addEventListener("input", () => this.schedule());
    field.addEventListener("keydown", (event) => {
      if (event instanceof KeyboardEvent) this.onKey(event);
    });
    field.addEventListener("blur", () => this.close());
    this.list.addEventListener("mousedown", (event) => {
      event.preventDefault();
      const option = event.target.closest("[role=option]");
      const index = option ? Number(option.getAttribute("data-index")) : -1;
      if (index >= 0) this.accept(index);
    });
  }
  schedule() {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.update(), DEBOUNCE_MS);
  }
  async update() {
    const caret = this.field.selectionStart ?? this.field.value.length;
    const found = target(this.field.value, caret, this.mode);
    this.current = found;
    this.request?.abort();
    if (!found) {
      this.show([]);
      return;
    }
    if (found.kind === "metatag-value") {
      this.show(metatagValues(found.metatag ?? "", found.typed));
      return;
    }
    const request = new AbortController();
    this.request = request;
    let suggestions;
    try {
      suggestions = await fetchSuggestions(found.typed, request.signal);
    } catch {
      return;
    }
    if (request.signal.aborted) return;
    const items = suggestions.map((s) => ({
      text: s.name,
      finished: true,
      category: s.category,
      count: s.post_count,
      ...s.antecedent === void 0 ? {} : { antecedent: s.antecedent }
    }));
    if (this.mode === "search") items.push(...metatagNames(found.typed).slice(0, 3));
    this.show(items);
  }
  show(items) {
    this.items = items;
    this.active = -1;
    this.list.replaceChildren(
      ...items.map((item, index) => {
        const option = document.createElement("li");
        option.id = `${this.list.id}-${index}`;
        option.setAttribute("role", "option");
        option.setAttribute("aria-selected", "false");
        option.dataset["index"] = String(index);
        if (item.antecedent) {
          const from = document.createElement("span");
          from.className = "antecedent";
          from.textContent = `${item.antecedent} \u2192`;
          option.append(from);
        }
        const name = document.createElement("span");
        name.className = item.category ? `tag tag-${item.category}` : "metatag";
        name.textContent = item.text;
        option.append(name);
        if (item.count !== void 0) {
          const count = document.createElement("span");
          count.className = "count";
          count.textContent = String(item.count);
          option.append(count);
        }
        return option;
      })
    );
    const open = items.length > 0;
    this.list.hidden = !open;
    this.field.setAttribute("aria-expanded", String(open));
    this.field.removeAttribute("aria-activedescendant");
  }
  close() {
    window.clearTimeout(this.timer);
    this.request?.abort();
    this.show([]);
  }
  highlight(index) {
    const options = this.list.children;
    for (let i = 0; i < options.length; i++) {
      options[i]?.setAttribute("aria-selected", String(i === index));
    }
    this.active = index;
    const option = options[index];
    if (option) {
      this.field.setAttribute("aria-activedescendant", option.id);
      option.scrollIntoView({ block: "nearest" });
    } else {
      this.field.removeAttribute("aria-activedescendant");
    }
  }
  onKey(event) {
    const open = !this.list.hidden;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        if (!open) return;
        event.preventDefault();
        const count = this.items.length;
        let next = this.active + (event.key === "ArrowDown" ? 1 : -1);
        if (next >= count) next = -1;
        if (next < -1) next = count - 1;
        this.highlight(next);
        return;
      }
      // Enter takes a highlighted suggestion (otherwise it submits); Tab
      // takes the highlighted or else the first one.
      case "Enter":
      case "Tab": {
        const index = this.active >= 0 || event.key === "Enter" ? this.active : 0;
        if (open && index >= 0 && !event.shiftKey) {
          event.preventDefault();
          this.accept(index);
        }
        return;
      }
      case "Escape":
        if (open) {
          event.preventDefault();
          this.close();
        }
        return;
    }
  }
  accept(index) {
    const item = this.items[index];
    const found = this.current;
    if (!item || !found) return;
    const value = this.field.value;
    const after = value.slice(found.end);
    const space = item.finished && !/^\s/.test(after) ? " " : "";
    const inserted = item.text + space;
    this.field.value = value.slice(0, found.start) + inserted + after;
    const caret = found.start + inserted.length;
    this.field.setSelectionRange(caret, caret);
    this.close();
    if (!item.finished) this.schedule();
  }
};
function attachAll(root = document) {
  for (const field of root.querySelectorAll("input[data-autocomplete], textarea[data-autocomplete]")) {
    const mode = field.dataset["autocomplete"] === "search" ? "search" : "tags";
    new Autocomplete(field, mode);
  }
}

// src/keyboard.ts
var SHORTCUTS = [
  ["a, \u2190", "Previous post or page"],
  ["d, \u2192", "Next post or page"],
  ["e", "Edit the post"],
  ["f", "Favorite the post"],
  ["n", "Show or hide notes"],
  ["/", "Search"],
  ["?", "Show these shortcuts"]
];
function actionFor(key) {
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
function busy(target2) {
  if (!(target2 instanceof Element)) return false;
  return target2.closest("input, textarea, select, button, video, audio, [contenteditable]") !== null || document.querySelector("dialog[open]") !== null;
}
function showHelp() {
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
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) dialog.close();
    });
    document.body.append(dialog);
  }
  dialog.showModal();
}
function run(action) {
  switch (action) {
    case "prev":
    case "next": {
      const link = document.querySelector(`a[rel=${action}]`);
      if (!link) return false;
      window.location.assign(link.href);
      return true;
    }
    case "edit": {
      const details = document.querySelector("details#edit");
      if (!details) return false;
      details.open = true;
      details.querySelector("textarea")?.focus();
      return true;
    }
    case "favorite": {
      const button = document.querySelector(".favorite button");
      if (!button) return false;
      button.click();
      return true;
    }
    case "notes": {
      const button = document.querySelector("[data-notes-toggle]");
      if (!button) return false;
      button.click();
      return true;
    }
    case "search": {
      const input = document.querySelector(".site-header input[name=tags]");
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
function enableShortcuts() {
  document.addEventListener("keydown", (event) => {
    if (event.ctrlKey || event.metaKey || event.altKey || busy(event.target)) return;
    const action = actionFor(event.key);
    if (action && run(action)) event.preventDefault();
  });
}

// src/note-editor.ts
function boxBetween(a, b, width, height) {
  const clamp = (v, max) => Math.min(Math.max(Math.round(v), 0), max);
  const [x1, x2] = [clamp(Math.min(a.x, b.x), width), clamp(Math.max(a.x, b.x), width)];
  const [y1, y2] = [clamp(Math.min(a.y, b.y), height), clamp(Math.max(a.y, b.y), height)];
  return { x: x1, y: y1, width: x2 - x1, height: y2 - y1 };
}
function moved(box, dx, dy, width, height) {
  const x = Math.min(Math.max(Math.round(box.x + dx), 0), Math.max(width - box.width, 0));
  const y = Math.min(Math.max(Math.round(box.y + dy), 0), Math.max(height - box.height, 0));
  return { ...box, x, y };
}
function resized(box, dx, dy, width, height) {
  const w = Math.min(Math.max(Math.round(box.width + dx), 1), width - box.x);
  const h = Math.min(Math.max(Math.round(box.height + dy), 1), height - box.y);
  return { ...box, width: w, height: h };
}
var MIN_DRAG = 4;
var HANDLE = 12;
async function send(method, url, body) {
  const init = {
    method,
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    credentials: "same-origin"
  };
  if (body !== void 0) init.body = JSON.stringify(body);
  const response = await fetch(url, init);
  if (response.ok) return null;
  try {
    const error = await response.json();
    return error.error?.message ?? `Error ${response.status}`;
  } catch {
    return `Error ${response.status}`;
  }
}
function enableNoteEditor(root = document) {
  const layer = root.querySelector("[data-notes-editable]");
  const svg = layer?.querySelector("svg.notes");
  const post = layer?.dataset["post"];
  if (!layer || !svg || !post) return;
  const [, , width, height] = (svg.getAttribute("viewBox") ?? "0 0 1 1").split(" ").map(Number);
  const toImage = (event) => {
    const box = svg.getBoundingClientRect();
    return {
      x: (event.clientX - box.left) / box.width * width,
      y: (event.clientY - box.top) / box.height * height
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
  const text = form.querySelector("textarea");
  const error = form.querySelector(".form-error");
  const deleteButton = form.querySelector("[data-delete]");
  let editing = null;
  let draft = null;
  function closeForm() {
    form.hidden = true;
    editing = null;
    draft?.remove();
    draft = null;
  }
  const openForm = (target2) => {
    editing = target2;
    text.value = target2.body ?? "";
    error.hidden = true;
    deleteButton.hidden = target2.id === void 0;
    form.hidden = false;
    text.focus();
  };
  const done = (message) => {
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
    if (editing.id === void 0) {
      void send("POST", `/api/v1/posts/${post}/notes`, { ...editing.box, body }).then(done);
    } else {
      void send("PUT", `/api/v1/notes/${editing.id}`, { body, base_version: Number(editing.version) }).then(done);
    }
  });
  form.querySelector("[data-cancel]").addEventListener("click", closeForm);
  deleteButton.addEventListener("click", () => {
    if (!editing?.id) return;
    void send("DELETE", `/api/v1/notes/${editing.id}?base_version=${editing.version}`).then(done);
  });
  const boxOf = (rect) => ({
    x: Number(rect.getAttribute("x")),
    y: Number(rect.getAttribute("y")),
    width: Number(rect.getAttribute("width")),
    height: Number(rect.getAttribute("height"))
  });
  const draw = (rect, box) => {
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
    const target2 = event.target.closest("rect.note-box");
    let mode;
    let original;
    let rect;
    if (target2 && !target2.classList.contains("draft")) {
      rect = target2;
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
    const onMove = (move) => {
      const at = toImage(move);
      const [dx, dy] = [at.x - start.x, at.y - start.y];
      if (Math.hypot(dx, dy) * scale() >= MIN_DRAG) dragged = true;
      if (!dragged) return;
      current = mode === "draw" ? boxBetween(start, at, width, height) : mode === "move" ? moved(original, dx, dy, width, height) : resized(original, dx, dy, width, height);
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

// src/notes.ts
var HIDDEN_KEY = "moekura:notes-hidden";
function remembered() {
  try {
    return localStorage.getItem(HIDDEN_KEY) === "1";
  } catch {
    return false;
  }
}
function remember(hidden) {
  try {
    if (hidden) localStorage.setItem(HIDDEN_KEY, "1");
    else localStorage.removeItem(HIDDEN_KEY);
  } catch {
  }
}
function popupPosition(box, layerWidth, popupWidth) {
  const left = Math.max(0, Math.min(box.left, layerWidth - popupWidth));
  return { left, top: box.top + box.height + 4 };
}
function enableNotes(root = document) {
  const layer = root.querySelector("[data-notes]");
  const svg = layer?.querySelector("svg.notes");
  if (!layer || !svg) return;
  const popup = root.createElement("div");
  popup.className = "note-popup";
  popup.hidden = true;
  popup.setAttribute("role", "tooltip");
  layer.append(popup);
  let shown = null;
  const hide = () => {
    popup.hidden = true;
    shown?.classList.remove("active");
    shown = null;
  };
  const show = (rect) => {
    if (layer.classList.contains("editing-notes")) return;
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
        height: rectBox.height
      },
      layerBox.width,
      popup.offsetWidth
    );
    popup.style.left = `${place.left}px`;
    popup.style.top = `${place.top}px`;
  };
  for (const rect of svg.querySelectorAll("rect.note-box")) {
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
  const apply = (hidden) => {
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

// src/pool-order.ts
function reorder(ids, moved2, before) {
  const rest = ids.filter((id) => id !== moved2);
  const at = before === null ? -1 : rest.indexOf(before);
  if (at < 0) return [...rest, moved2];
  return [...rest.slice(0, at), moved2, ...rest.slice(at)];
}
function enablePoolOrder(root = document) {
  const list = root.querySelector("[data-pool-order]");
  const field = root.querySelector("[data-pool-posts]");
  if (!list || !field) return;
  let dragged = null;
  for (const item of list.querySelectorAll("li[data-id]")) {
    item.draggable = true;
    item.addEventListener("dragstart", (event) => {
      dragged = item;
      item.classList.add("dragging");
      event.dataTransfer?.setData("text/plain", item.dataset["id"] ?? "");
    });
    item.addEventListener("dragend", () => {
      item.classList.remove("dragging");
      dragged = null;
    });
    item.querySelector("a")?.addEventListener("click", (event) => event.preventDefault());
  }
  list.addEventListener("dragover", (event) => {
    if (!dragged) return;
    event.preventDefault();
    const target2 = event.target.closest("li[data-id]");
    if (!target2 || target2 === dragged) return;
    const box = target2.getBoundingClientRect();
    const after = event.clientX > box.left + box.width / 2;
    list.insertBefore(dragged, after ? target2.nextSibling : target2);
  });
  list.addEventListener("drop", (event) => {
    event.preventDefault();
    if (!dragged) return;
    const moved2 = dragged.dataset["id"] ?? "";
    const next = dragged.nextElementSibling;
    const ids = field.value.split(/[\s,]+/).filter((id) => id !== "").map((id) => id.replace(/^#/, ""));
    field.value = reorder(ids, moved2, next?.dataset["id"] ?? null).join(" ");
  });
}

// src/reactions.ts
function update(root, state) {
  const score = root.querySelector(".vote .score");
  if (score) score.textContent = String(state.score);
  const favCount = root.querySelector(".favorite .fav-count");
  if (favCount && state.fav_count !== void 0) favCount.textContent = String(state.fav_count);
  for (const button of root.querySelectorAll(".vote button[name=score]")) {
    const direction = button.getAttribute("aria-label") === "Vote up" ? 1 : -1;
    const pressed = state.vote === direction;
    button.setAttribute("aria-pressed", String(pressed));
    button.value = String(pressed ? 0 : direction);
  }
  const favorite = root.querySelector(".favorite button[name=favorite]");
  if (favorite && state.favorited !== void 0) {
    favorite.setAttribute("aria-pressed", String(state.favorited));
    favorite.value = state.favorited ? "remove" : "add";
  }
}
function enhanceReactions(root = document) {
  for (const form of root.querySelectorAll("form[data-reaction]")) {
    form.addEventListener("submit", (event) => {
      if (form.dataset["plain"]) return;
      event.preventDefault();
      const submitter = event.submitter instanceof HTMLButtonElement ? event.submitter : null;
      const body = new URLSearchParams();
      for (const [name, value] of new FormData(form, submitter)) {
        if (typeof value === "string") body.append(name, value);
      }
      void fetch(form.getAttribute("action") ?? "", {
        method: "POST",
        body,
        headers: { Accept: "application/json" }
      }).then(async (response) => {
        if (!response.ok) throw new Error(String(response.status));
        const scope = form.closest(".comment") ?? form.closest(".post-info") ?? root;
        update(scope, await response.json());
      }).catch(() => {
        form.dataset["plain"] = "1";
        form.requestSubmit(submitter);
      });
    });
  }
}

// src/reader.ts
var PREFIX = "moekura:read:";
function load(pool) {
  try {
    const value = Number(localStorage.getItem(PREFIX + pool));
    return Number.isInteger(value) && value > 0 ? value : null;
  } catch {
    return null;
  }
}
function save(pool, page) {
  try {
    localStorage.setItem(PREFIX + pool, String(page));
  } catch {
  }
}
function enableReader(root = document) {
  const reader = root.querySelector("[data-reader]");
  const pool = reader?.dataset["reader"];
  if (reader && pool) {
    const pages = [...reader.querySelectorAll("[data-page]")];
    const [only] = pages;
    if (pages.length === 1 && only) {
      save(pool, Number(only.dataset["page"]));
    } else if (pages.length > 1 && "IntersectionObserver" in window) {
      const observer = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (entry.isIntersecting) save(pool, Number(entry.target.dataset["page"]));
          }
        },
        { threshold: 0.5 }
      );
      for (const page of pages) observer.observe(page);
    }
  }
  const resume = root.querySelector("[data-reader-resume]");
  const resumePool = resume?.dataset["readerResume"];
  const link = resume?.querySelector("a");
  if (resume && resumePool && link) {
    const page = load(resumePool);
    if (page !== null && page > 1) {
      link.href = `/pools/${resumePool}/read/${page}`;
      link.textContent = `Continue reading from page ${page}`;
      resume.hidden = false;
    }
  }
}

// src/suggestions.ts
function withTag(tags, tag) {
  const words = tags.split(/\s+/).filter((word) => word !== "");
  if (words.includes(tag)) return tags;
  const kept = tags.trimEnd();
  return kept === "" ? `${tag} ` : `${kept} ${tag} `;
}
function enableSuggestions(root = document) {
  const box = root.querySelector("[data-suggestions]");
  const form = box?.closest("form");
  const field = form?.querySelector("textarea[name=tags]");
  if (!box || !form || !field) return;
  const hint = box.querySelector("[data-suggestions-hint]");
  if (hint) hint.textContent = "Clicking one adds it to the form; save to keep it.";
  box.addEventListener("click", (event) => {
    const button = event.target.closest("button[name]");
    if (!button) return;
    event.preventDefault();
    if (button.name === "add") {
      field.value = withTag(field.value, button.value);
    } else if (button.name === "suggested_rating") {
      for (const radio of form.querySelectorAll("input[name=rating]")) {
        radio.checked = radio.value === button.value;
      }
    }
    button.classList.add("chosen");
    button.disabled = true;
  });
}

// src/tag-script.ts
var RATINGS = {
  g: "g",
  general: "g",
  s: "s",
  sensitive: "s",
  q: "q",
  questionable: "q",
  e: "e",
  explicit: "e"
};
function parseScript(text) {
  const script = { add: [], remove: [], rating: null };
  for (const word of text.trim().split(/\s+/)) {
    if (word === "") continue;
    const lower = word.toLowerCase();
    if (lower.startsWith("rating:")) {
      const rating = RATINGS[lower.slice("rating:".length)];
      if (!rating) throw new Error(`Unknown rating in \u201C${word}\u201D.`);
      script.rating = rating;
    } else if (word.startsWith("-") && word.length > 1) {
      script.remove.push(word.slice(1));
    } else {
      script.add.push(word);
    }
  }
  return script;
}
function postId(href) {
  return /\/posts\/(\d+)/.exec(href)?.[1] ?? null;
}
function enableTagScript(root = document) {
  const panel = root.querySelector("[data-tag-script]");
  const input = panel?.querySelector("input");
  const toggle = panel?.querySelector("input[type=checkbox]");
  const status = panel?.querySelector("[data-tag-script-status]");
  const text = panel?.querySelector("input[type=text]");
  if (!panel || !input || !toggle || !status || !text) return;
  panel.hidden = false;
  const say = (message) => {
    status.textContent = message;
  };
  root.addEventListener(
    "click",
    (event) => {
      if (!toggle.checked) return;
      const card = event.target.closest(".post-grid a.card");
      if (!card) return;
      event.preventDefault();
      const id = postId(card.getAttribute("href") ?? "");
      if (!id) return;
      let script;
      try {
        script = parseScript(text.value);
      } catch (error) {
        say(error.message);
        return;
      }
      if (script.add.length === 0 && script.remove.length === 0 && script.rating === null) {
        say("Type a script first.");
        return;
      }
      const body = { add_tags: script.add, remove_tags: script.remove };
      if (script.rating) body["rating"] = script.rating;
      card.classList.remove("script-ok", "script-failed");
      card.classList.add("script-busy");
      void fetch(`/api/v1/posts/${id}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        credentials: "same-origin",
        body: JSON.stringify(body)
      }).then(async (response) => {
        card.classList.remove("script-busy");
        if (response.ok) {
          card.classList.add("script-ok");
          say(`Post #${id} changed.`);
        } else {
          card.classList.add("script-failed");
          const error = await response.json().catch(() => null);
          say(`Post #${id}: ${error?.error?.message ?? `error ${response.status}`}`);
        }
      }).catch(() => {
        card.classList.remove("script-busy");
        card.classList.add("script-failed");
        say(`Post #${id} couldn't be changed.`);
      });
    },
    true
  );
}

// src/main.ts
document.documentElement.classList.add("js");
attachAll();
enhanceReactions();
enableShortcuts();
enablePoolOrder();
enableReader();
enableNotes();
enableNoteEditor();
enableTagScript();
enableSuggestions();
