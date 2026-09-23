// Built from frontend/src by npm run build. Do not edit.

// src/metatags.ts
var METATAGS = {
  id: [],
  rating: ["general", "sensitive", "questionable", "explicit"],
  status: ["pending", "active", "flagged", "deleted", "any"],
  user: [],
  score: [],
  favcount: [],
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
    "random"
  ],
  limit: []
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

// src/main.ts
document.documentElement.classList.add("js");
attachAll();
