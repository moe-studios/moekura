// Built from frontend/src by npm run build. Do not edit.

// src/i18n.ts
var messages = null;
function load() {
  if (messages === null) {
    try {
      const data = typeof document === "undefined" ? void 0 : document.body?.dataset.messages;
      messages = data ? JSON.parse(data) : {};
    } catch {
      messages = {};
    }
  }
  return messages ?? {};
}
function t(key, english, args = {}) {
  let text = load()[`js-${key}`] ?? english;
  for (const [name, value] of Object.entries(args)) {
    text = text.replaceAll(`{$${name}}`, String(value));
  }
  return text;
}

// src/suggestions.ts
var POLL_MS = 3e3;
var POLL_FOR_MS = 5 * 60 * 1e3;
function withTag(tags, tag) {
  const words = tags.split(/\s+/).filter((word) => word !== "");
  if (words.includes(tag)) return tags;
  const kept = tags.trimEnd();
  return kept === "" ? `${tag} ` : `${kept} ${tag} `;
}
function hasTag(tags, tag) {
  return tags.split(/\s+/).some((word) => word.toLowerCase() === tag || word.toLowerCase().endsWith(`:${tag}`));
}
function enableSuggestions(root = document) {
  const box = root.querySelector("[data-suggestions]");
  const form = box?.closest("form");
  const field = form?.querySelector("textarea[name=tags]");
  if (!box || !form || !field) return;
  const upload = box.dataset["suggestions"] === "upload";
  const prepare = () => {
    const hint = box.querySelector("[data-suggestions-hint]");
    if (hint && !upload) hint.textContent = t("suggestions-hint", "Clicking one adds it to the form; save to keep it.");
    const rating = form.querySelector("input[name=rating]:checked")?.value;
    for (const button of box.querySelectorAll("button[name]")) {
      const taken = button.name === "add" && hasTag(field.value, button.value) || button.name === "suggested_rating" && button.value === rating;
      if (taken) {
        button.classList.add("chosen");
        button.disabled = true;
      }
    }
    for (const check of box.querySelectorAll("[data-suggestions-check]")) check.hidden = true;
  };
  prepare();
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
  const url = box.dataset["suggestionsPoll"];
  if (!url) return;
  const started = Date.now();
  const poll = async () => {
    try {
      const response = await fetch(url, { headers: { Accept: "text/html" } });
      if (response.ok) {
        const fetched = new DOMParser().parseFromString(await response.text(), "text/html").body;
        if (!fetched.querySelector("[data-suggestions-pending]")) {
          if (fetched.querySelector("button[name]")) {
            box.replaceChildren(...Array.from(fetched.childNodes, (node) => root.importNode(node, true)));
            prepare();
          } else {
            box.hidden = true;
          }
          return;
        }
      }
    } catch {
    }
    if (Date.now() - started < POLL_FOR_MS) window.setTimeout(() => void poll(), POLL_MS);
  };
  window.setTimeout(() => void poll(), POLL_MS);
}

// src/artist-finder.ts
var DEBOUNCE_MS = 400;
function firstUrl(values) {
  for (const value of values) {
    const trimmed = value.trim();
    if (/^https?:\/\/\S+$/i.test(trimmed)) return trimmed;
  }
  return null;
}
function enableArtistFinder(root = document) {
  const box = root.querySelector("[data-artist-finder]");
  const form = box?.closest("form");
  const tags = form?.querySelector("textarea[name=tags]");
  if (!box || !form || !tags) return;
  const inputs = (box.dataset["artistFinder"] ?? "").split(/\s+/).map((name) => form.querySelector(`input[name="${name}"]`)).filter((input) => input !== null);
  let timer;
  let request;
  let last = "";
  const show = ({ artists: found, unknown }) => {
    if (found.length === 0 && unknown) {
      const label2 = document.createElement("span");
      label2.className = "hint";
      label2.textContent = t("artist-finder-unknown", "By {$name}, who has no artist entry yet: ", { name: unknown.name });
      const link = document.createElement("a");
      link.href = unknown.new_url;
      link.target = "_blank";
      link.textContent = t("artist-finder-start", "start one");
      box.replaceChildren(label2, link);
      box.hidden = false;
      return;
    }
    if (found.length === 0) {
      box.hidden = true;
      box.replaceChildren();
      return;
    }
    const label = document.createElement("span");
    label.className = "hint";
    label.textContent = found.length === 1 ? t("artist-finder-one", "Artist: ") : t("artist-finder-many", "Artists: ");
    const buttons = found.map((artist) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "tag tag-artist link";
      button.dataset["tag"] = artist.name;
      button.textContent = artist.name;
      button.title = t("artist-finder-add", "Add to the tags");
      return button;
    });
    box.replaceChildren(label, ...buttons);
    box.hidden = false;
  };
  const update2 = async () => {
    const url = firstUrl(inputs.map((input) => input.value));
    if (url === null) {
      last = "";
      show({ artists: [] });
      return;
    }
    if (url === last) return;
    last = url;
    request?.abort();
    request = new AbortController();
    try {
      const response = await fetch(`/artists/finder?${new URLSearchParams({ url }).toString()}`, {
        signal: request.signal,
        headers: { Accept: "application/json" }
      });
      if (!response.ok) return;
      show(await response.json());
    } catch {
    }
  };
  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void update2(), DEBOUNCE_MS);
  };
  for (const input of inputs) input.addEventListener("input", schedule);
  box.addEventListener("click", (event) => {
    const tag = event.target.closest("button[data-tag]")?.dataset["tag"];
    if (!tag) return;
    tags.value = withTag(tags.value, tag);
    tags.dispatchEvent(new Event("input", { bubbles: true }));
  });
  void update2();
}

// src/metatags.ts
var METATAGS = {
  id: [],
  rating: ["general", "sensitive", "questionable", "explicit"],
  status: ["pending", "active", "flagged", "deleted", "modqueue", "unmoderated", "appealed", "any"],
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
    "gentags",
    "arttags",
    "copytags",
    "chartags",
    "metatags",
    "random",
    "comment",
    "comment_asc",
    "note",
    "note_asc",
    "rank",
    "change",
    "change_asc",
    "upvotes",
    "upvotes_asc",
    "downvotes",
    "downvotes_asc",
    "comment_bumped",
    "comment_bumped_asc",
    "comment_count",
    "comment_count_asc",
    "note_count",
    "note_count_asc",
    "custom",
    "md5",
    "md5_asc"
  ],
  limit: [],
  fav: [],
  ordfav: [],
  similar: [],
  pool: ["any", "none"],
  ordpool: [],
  search: ["all"],
  favgroup: ["any", "none"],
  ordfavgroup: [],
  ai: [],
  child: ["any", "none"],
  is: [
    "parent",
    "child",
    "sfw",
    "nsfw",
    "general",
    "sensitive",
    "questionable",
    "explicit",
    "pending",
    "active",
    "flagged",
    "deleted",
    "modqueue",
    "unmoderated",
    "appealed"
  ],
  source: ["any", "none"],
  approver: ["any", "none"],
  commenter: [],
  comment: [],
  commentary: ["true", "false", "translated", "untranslated"],
  exif: [],
  embedded: ["true", "false"],
  pixiv: ["any", "none"],
  pixiv_id: [],
  noter: [],
  flagger: [],
  gentags: [],
  arttags: [],
  copytags: [],
  chartags: [],
  metatags: [],
  age: [],
  updated: [],
  upvote: [],
  downvote: [],
  has: ["source", "children", "parent", "pools", "notes", "comments"]
};
var CATEGORIES = ["artist", "copyright", "character", "general", "meta"];
var EDIT_METATAGS = {
  rating: ["general", "sensitive", "questionable", "explicit"],
  parent: ["none"],
  child: [],
  source: ["none"],
  pool: [],
  newpool: [],
  fav: [],
  favgroup: [],
  upvote: [],
  downvote: []
};
var NEGATABLE = ["parent", "child", "pool", "fav", "favgroup"];

// src/autocomplete.ts
function metatagsOf(mode) {
  if (mode === "search") return METATAGS;
  if (mode === "edit") return EDIT_METATAGS;
  return {};
}
var DEBOUNCE_MS2 = 120;
var cache = /* @__PURE__ */ new Map();
var nextId = 0;
function target(value, caret, mode) {
  let start = caret;
  while (start > 0 && !/\s/.test(value.charAt(start - 1))) start--;
  let end = caret;
  while (end < value.length && !/\s/.test(value.charAt(end))) end++;
  let word = value.slice(start, caret);
  if (mode === "search") {
    while (/^[-~(]/.test(word)) {
      start += 1;
      word = word.slice(1);
    }
    const unbalanced = (s) => s.split(")").length > s.split("(").length;
    if (word.endsWith(")") && unbalanced(word)) return null;
    while (end > caret && value.charAt(end - 1) === ")" && unbalanced(value.slice(start, end))) end--;
  }
  if (mode === "edit" && word.startsWith("-")) {
    start += 1;
    word = word.slice(1);
  }
  const colon = word.indexOf(":");
  if (colon > 0) {
    const prefix = word.slice(0, colon).toLowerCase();
    const rest = word.slice(colon + 1);
    if (mode !== "tags" && prefix in metatagsOf(mode)) {
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
function metatagNames(typed, mode, negated = false) {
  const lower = typed.toLowerCase();
  return Object.keys(metatagsOf(mode)).filter((name) => name.startsWith(lower) && (!negated || mode !== "edit" || NEGATABLE.includes(name))).map((name) => ({ text: `${name}:`, finished: false }));
}
function metatagValues(mode, metatag, typed) {
  const lower = typed.toLowerCase();
  return (metatagsOf(mode)[metatag] ?? []).filter((value) => value.startsWith(lower) && value !== lower).map((value) => ({ text: value, finished: true }));
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
    this.timer = window.setTimeout(() => void this.update(), DEBOUNCE_MS2);
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
      this.show(metatagValues(this.mode, found.metatag ?? "", found.typed));
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
    if (this.mode !== "tags") {
      const negated = this.field.value.charAt(found.start - 1) === "-";
      items.push(...metatagNames(found.typed, this.mode, negated).slice(0, 3));
    }
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
    const wanted = field.dataset["autocomplete"];
    const mode = wanted === "search" || wanted === "edit" ? wanted : "tags";
    new Autocomplete(field, mode);
  }
}

// src/autosubmit.ts
function enableAutosubmit() {
  document.addEventListener("change", (event) => {
    const target2 = event.target;
    if (!(target2 instanceof HTMLSelectElement)) return;
    target2.closest("form[data-autosubmit]")?.requestSubmit();
  });
}

// src/confirm.ts
function questionFor(form, submitter) {
  return submitter?.getAttribute("data-confirm") ?? form.getAttribute("data-confirm");
}
function enableConfirm() {
  document.addEventListener(
    "submit",
    (event) => {
      const question = questionFor(event.target, event.submitter);
      if (question !== null && !window.confirm(question)) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    },
    true
  );
}

// src/copy-tags.ts
function withTags(tags, copied) {
  return copied.split(/\s+/).filter((word) => word !== "").reduce((all, word) => withTag(all, word), tags);
}
function enableCopyTags(root = document) {
  const box = root.querySelector("[data-copy-tags]");
  const field = box?.closest("form")?.querySelector("textarea[name=tags]");
  if (!box || !field) return;
  const hint = box.querySelector("[data-copy-tags-hint]");
  if (hint) hint.textContent = t("copy-tags-hint", "Clicking one adds that post's tags to the form; save to keep them.");
  box.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-tags]");
    if (!button) return;
    event.preventDefault();
    field.value = withTags(field.value, button.dataset["tags"] ?? "");
    button.classList.add("chosen");
    button.disabled = true;
  });
}

// src/keyboard.ts
var SHORTCUTS = [
  ["a, \u2190", "previous", "Previous post or page"],
  ["d, \u2192", "next", "Next post or page"],
  ["e", "edit", "Edit the post"],
  ["f", "favorite", "Favorite the post"],
  ["n", "notes", "Show or hide notes"],
  ["/", "search", "Search"],
  ["?", "help", "Show these shortcuts"]
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
      const input = document.querySelector("form[role=search] input[name=tags]");
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

// src/layout.ts
function reveal(target2) {
  if (!(target2 instanceof HTMLElement)) return;
  const details = target2.closest("details");
  if (details) details.open = true;
  target2.focus();
}
function enableLayout() {
  for (const link of document.querySelectorAll("a[data-open-edit]")) {
    link.addEventListener("click", (event) => {
      event.preventDefault();
      reveal(document.querySelector(link.hash));
    });
  }
  if (window.location.hash === "#edit-tags") reveal(document.getElementById("edit-tags"));
  const shortcuts = document.querySelector("[data-shortcuts-link]");
  if (shortcuts) {
    shortcuts.hidden = false;
    shortcuts.querySelector("button")?.addEventListener("click", showHelp);
  }
}

// src/toast.ts
var SHOW_FOR_MS = 6e3;
function enableToasts() {
  const list = document.createElement("ul");
  list.id = "toasts";
  list.className = "toasts";
  list.setAttribute("role", "status");
  document.body.append(list);
}
function toast(message) {
  const region = document.getElementById("toasts");
  if (!region) {
    window.alert(message);
    return;
  }
  const item = document.createElement("li");
  item.className = "toast";
  item.textContent = message;
  region.append(item);
  window.setTimeout(() => item.remove(), SHOW_FOR_MS);
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
    return error.error?.message ?? t("error-status", "Error {$status}", { status: response.status });
  } catch {
    return t("error-status", "Error {$status}", { status: response.status });
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
  toggle.textContent = t("notes-edit", "Edit notes");
  toggle.setAttribute("aria-pressed", "false");
  (root.querySelector("[data-notes-toggle]") ?? layer).after(toggle);
  toggle.addEventListener("click", () => {
    const on = !layer.classList.contains("editing-notes");
    layer.classList.toggle("editing-notes", on);
    layer.classList.remove("notes-hidden");
    toggle.setAttribute("aria-pressed", String(on));
    toggle.textContent = on ? t("notes-done", "Done editing notes") : t("notes-edit", "Edit notes");
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
          toast(message);
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
function embeddedPlace(box, imageWidth, imageHeight) {
  const percent = (n, of) => `${(n / of * 100).toFixed(3)}%`;
  return {
    left: percent(box.x, imageWidth),
    top: percent(box.y, imageHeight),
    width: percent(box.width, imageWidth),
    height: percent(box.height, imageHeight)
  };
}
function embed(root, layer, svg) {
  const [, , imageWidth = 1, imageHeight = 1] = (svg.getAttribute("viewBox") ?? "").split(/\s+/).map(Number);
  for (const rect of svg.querySelectorAll("rect.note-box")) {
    const text = root.querySelector(`[data-note-text="${rect.dataset["note"]}"] .markup`);
    if (!text) continue;
    const note = root.createElement("div");
    note.className = "note-embedded";
    note.dataset["embeddedNote"] = rect.dataset["note"] ?? "";
    note.append(text.cloneNode(true));
    const number = (name) => Number(rect.getAttribute(name) ?? 0);
    Object.assign(
      note.style,
      embeddedPlace(
        { x: number("x"), y: number("y"), width: number("width"), height: number("height") },
        imageWidth,
        imageHeight
      )
    );
    layer.append(note);
  }
}
function enableNotes(root = document) {
  const layer = root.querySelector("[data-notes]");
  const svg = layer?.querySelector("svg.notes");
  if (!layer || !svg) return;
  const embedded = layer.dataset["notesEmbedded"] !== void 0;
  if (embedded) embed(root, layer, svg);
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
  for (const rect of embedded ? [] : svg.querySelectorAll("rect.note-box")) {
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
    toggle.textContent = hidden ? t("notes-show", "Show notes") : t("notes-hide", "Hide notes");
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
    const direction = button.dataset.vote === "up" ? 1 : -1;
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
        if (response.status === 429) {
          toast(t("too-quick", "That was too quick. Wait a moment, then try again."));
          return;
        }
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
function load2(pool) {
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
    const page = load2(resumePool);
    if (page !== null && page > 1) {
      link.href = `/pools/${resumePool}/read/${page}`;
      link.textContent = t("reader-continue", "Continue reading from page {$page}", { page });
      resume.hidden = false;
    }
  }
}

// src/related-tags.ts
var DEBOUNCE_MS3 = 400;
function chosenTag(value, caret) {
  let start = caret;
  while (start > 0 && !/\s/.test(value.charAt(start - 1))) start--;
  let end = caret;
  while (end < value.length && !/\s/.test(value.charAt(end))) end++;
  const word = value.slice(start, end);
  if (word === "" || word.startsWith("-") || word.includes(":") || word.includes("*")) return null;
  return word.toLowerCase();
}
function withoutTag(tags, tag) {
  const words = tags.split(/\s+/).filter((word) => word !== "" && word.toLowerCase() !== tag);
  return words.length === 0 ? "" : `${words.join(" ")} `;
}
function hasTag2(tags, tag) {
  return tags.split(/\s+/).some((word) => word.toLowerCase() === tag);
}
function sourceOf(field) {
  const form = field.form;
  if (!form) return null;
  for (const name of ["url", "source"]) {
    const input = form.querySelector(`input[name="${name}"]`);
    const value = input?.value.trim() ?? "";
    if (/^https?:\/\/\S+$/i.test(value)) return value;
  }
  return null;
}
function enableRelatedTags(root = document) {
  for (const panel of root.querySelectorAll("[data-related-tags]")) {
    const field = root.getElementById(panel.dataset["relatedTags"] ?? "");
    if (field instanceof HTMLTextAreaElement) attach(panel, field);
  }
}
function attach(panel, field) {
  const list = document.createElement("div");
  list.className = "related-groups";
  list.setAttribute("aria-live", "polite");
  panel.append(list);
  let timer;
  let request;
  let lastUrl = "";
  const render = (groups) => {
    list.replaceChildren(
      ...groups.map((group) => {
        const section = document.createElement("section");
        section.className = `related-group related-${group.kind}`;
        const title = document.createElement("h3");
        title.textContent = group.title;
        const items = document.createElement("ul");
        for (const tag of group.tags) {
          const item = document.createElement("li");
          const button = document.createElement("button");
          button.type = "button";
          button.className = `tag tag-${tag.category}`;
          button.dataset["tag"] = tag.name;
          button.setAttribute("aria-pressed", String(hasTag2(field.value, tag.name)));
          button.textContent = tag.name;
          button.title = tag.from ? `${tag.from} \u2192 ${tag.name}` : t("related-posts", "{$count} posts", { count: tag.post_count });
          item.append(button);
          items.append(item);
        }
        section.append(title, items);
        return section;
      })
    );
  };
  const update2 = async () => {
    const chosen = chosenTag(field.value, field.selectionStart ?? field.value.length);
    const params = new URLSearchParams({ tags: field.value });
    if (chosen) params.set("tag", chosen);
    const source = sourceOf(field);
    if (source) params.set("source", source);
    const url = `/tags/related?${params.toString()}`;
    if (url === lastUrl) return;
    lastUrl = url;
    request?.abort();
    request = new AbortController();
    try {
      const response = await fetch(url, { signal: request.signal, headers: { Accept: "application/json" } });
      if (!response.ok) return;
      const panelData = await response.json();
      render(panelData.groups);
    } catch {
    }
  };
  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void update2(), DEBOUNCE_MS3);
  };
  field.addEventListener("input", schedule);
  for (const input of field.form?.querySelectorAll('input[name="url"], input[name="source"]') ?? []) {
    input.addEventListener("change", schedule);
  }
  field.addEventListener("click", schedule);
  field.addEventListener("keyup", (event) => {
    if (event.key.startsWith("Arrow") || event.key === "Home" || event.key === "End") schedule();
  });
  list.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-tag]");
    const tag = button?.dataset["tag"];
    if (!button || !tag) return;
    const present = hasTag2(field.value, tag);
    field.value = present ? withoutTag(field.value, tag) : withTag(field.value, tag);
    for (const other of list.querySelectorAll("button[data-tag]")) {
      if (other.dataset["tag"] === tag) other.setAttribute("aria-pressed", String(!present));
    }
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
  void update2();
}

// src/resized.ts
function enableResized() {
  const notice = document.querySelector("[data-resized]");
  const image = document.querySelector("[data-notes] img");
  const link = notice?.querySelector("a");
  const text = notice?.querySelector("span");
  const { sample, original, percent } = notice?.dataset ?? {};
  if (!notice || !image || !link || !text || !sample || !original) return;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    const showOriginal = image.getAttribute("src") !== original;
    image.src = showOriginal ? original : sample;
    text.textContent = showOriginal ? t("resized-original", "Showing the original.") : t("resized-to", "Resized to {$percent}% of the original.", { percent: percent ?? "" });
    link.textContent = showOriginal ? t("resized-show-resized", "Show the resized image") : t("resized-show-original", "View the original");
  });
}

// src/select-all.ts
function controlled(master) {
  const form = master.dataset.selectAll;
  if (!form) return [];
  return Array.from(document.querySelectorAll(`input[type=checkbox][form="${CSS.escape(form)}"]`));
}
function enableSelectAll() {
  for (const master of document.querySelectorAll("input[data-select-all]")) {
    master.addEventListener("change", () => {
      for (const box of controlled(master)) box.checked = master.checked;
    });
  }
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
      if (!rating) throw new Error(t("tag-script-rating", "Unknown rating in \u201C{$word}\u201D.", { word }));
      script.rating = rating;
    } else if (word.startsWith("-") && word.length > 1 && !isNegatedMetatag(lower)) {
      script.remove.push(word.slice(1));
    } else {
      script.add.push(word);
    }
  }
  return script;
}
function isNegatedMetatag(word) {
  const name = word.slice(1).split(":")[0] ?? "";
  return NEGATABLE.includes(name) && (word.includes(":") || name === "fav" || name === "parent");
}
function postId(href) {
  return /\/posts\/(\d+)/.exec(href)?.[1] ?? null;
}
var MODE_KEY = "moekura.tag-script.mode";
var SCRIPT_KEY = "moekura.tag-script.text";
function enableTagScript(root = document) {
  const panel = root.querySelector("[data-tag-script]");
  const mode = panel?.querySelector("select[data-tag-script-mode]");
  const field = panel?.querySelector("[data-tag-script-field]");
  const status = panel?.querySelector("[data-tag-script-status]");
  const text = panel?.querySelector("input[type=text]");
  if (!panel || !mode || !field || !status || !text) return;
  panel.hidden = false;
  const scripting = () => mode.value === "script";
  const show = () => {
    field.hidden = !scripting();
    root.querySelector(".post-grid")?.classList.toggle("scripting", scripting());
  };
  const stored = (key) => {
    try {
      return sessionStorage.getItem(key);
    } catch {
      return null;
    }
  };
  const store = (key, value) => {
    try {
      sessionStorage.setItem(key, value);
    } catch {
    }
  };
  if (stored(MODE_KEY) === "script") mode.value = "script";
  text.value = stored(SCRIPT_KEY) ?? text.value;
  show();
  mode.addEventListener("change", () => {
    store(MODE_KEY, mode.value);
    show();
    if (scripting()) text.focus();
  });
  text.addEventListener("input", () => store(SCRIPT_KEY, text.value));
  const say = (message) => {
    status.textContent = message;
  };
  root.addEventListener(
    "click",
    (event) => {
      if (!scripting()) return;
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
        say(t("tag-script-empty", "Type a script first."));
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
          say(t("tag-script-changed", "Post #{$id} changed.", { id }));
        } else {
          card.classList.add("script-failed");
          const error = await response.json().catch(() => null);
          say(
            t("tag-script-error", "Post #{$id}: {$error}", {
              id,
              error: error?.error?.message ?? t("error-status", "Error {$status}", { status: response.status })
            })
          );
        }
      }).catch(() => {
        card.classList.remove("script-busy");
        card.classList.add("script-failed");
        say(t("tag-script-failed", "Post #{$id} couldn't be changed.", { id }));
      });
    },
    true
  );
}

// src/upload.ts
function enableUpload(root = document) {
  const form = root.querySelector("form[data-upload]");
  const input = form?.querySelector('input[type="file"]');
  const zone = form?.querySelector("[data-upload-drop-zone]");
  const status = form?.querySelector("[data-upload-status]");
  if (!form || !input || !zone || !status) return;
  const max = Number(input.dataset["max"] ?? "1") || 1;
  let sending = false;
  const send2 = (files) => {
    if (sending || files.length === 0) return;
    if (files.length > max) {
      status.textContent = t("upload-too-many", "Choose at most {$max} files at once.", { max });
      return;
    }
    if (files !== input.files) {
      const transfer = new DataTransfer();
      for (const file of files) transfer.items.add(file);
      input.files = transfer.files;
    }
    status.textContent = files.length === 1 ? t("upload-sending-one", "Uploading {$name}\u2026", { name: files[0].name }) : t("upload-sending-many", "Uploading {$count} files\u2026", { count: files.length });
    sending = true;
    zone.classList.add("sending");
    form.requestSubmit();
  };
  input.addEventListener("change", () => {
    if (input.files) send2(input.files);
  });
  form.addEventListener("submit", () => {
    sending = true;
    for (const button of form.querySelectorAll("button[type=submit]")) button.disabled = true;
  });
  root.defaultView?.addEventListener("pageshow", () => {
    sending = false;
    zone.classList.remove("sending");
    for (const button of form.querySelectorAll("button[type=submit]")) button.disabled = false;
  });
  if (typeof DataTransfer === "undefined") return;
  zone.classList.add("enhanced");
  const hint = form.querySelector("[data-upload-hint]");
  if (hint) hint.hidden = false;
  root.addEventListener("paste", (event) => {
    const files = event.clipboardData?.files;
    if (!files?.length) return;
    event.preventDefault();
    send2(files);
  });
  const hasFiles = (event) => event.dataTransfer?.types.includes("Files") ?? false;
  let depth = 0;
  form.addEventListener("dragenter", (event) => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    depth++;
    zone.classList.add("dragging");
  });
  form.addEventListener("dragover", (event) => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });
  form.addEventListener("dragleave", () => {
    depth = Math.max(0, depth - 1);
    if (!depth) zone.classList.remove("dragging");
  });
  form.addEventListener("drop", (event) => {
    depth = 0;
    zone.classList.remove("dragging");
    if (!hasFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer?.files.length) send2(event.dataTransfer.files);
  });
}

// src/main.ts
document.documentElement.classList.add("js");
var off = (feature) => document.documentElement.dataset[feature] === "off";
enableToasts();
enableConfirm();
enableAutosubmit();
enableLayout();
if (!off("autocomplete")) attachAll();
enhanceReactions();
if (!off("shortcuts")) enableShortcuts();
enablePoolOrder();
enableReader();
enableResized();
enableNotes();
enableNoteEditor();
enableTagScript();
enableSuggestions();
enableCopyTags();
enableRelatedTags();
enableSelectAll();
enableUpload();
enableArtistFinder();
