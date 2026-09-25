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
    "comment_asc"
  ],
  limit: [],
  fav: [],
  ordfav: [],
  similar: [],
  pool: ["any", "none"],
  ordpool: []
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

// src/pool-order.ts
function reorder(ids, moved, before) {
  const rest = ids.filter((id) => id !== moved);
  const at = before === null ? -1 : rest.indexOf(before);
  if (at < 0) return [...rest, moved];
  return [...rest.slice(0, at), moved, ...rest.slice(at)];
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
    const moved = dragged.dataset["id"] ?? "";
    const next = dragged.nextElementSibling;
    const ids = field.value.split(/[\s,]+/).filter((id) => id !== "").map((id) => id.replace(/^#/, ""));
    field.value = reorder(ids, moved, next?.dataset["id"] ?? null).join(" ");
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

// src/main.ts
document.documentElement.classList.add("js");
attachAll();
enhanceReactions();
enableShortcuts();
enablePoolOrder();
