// Tag autocomplete for the search box and tag fields: completes the word
// under the cursor from /tags/autocomplete, following the ARIA combobox
// pattern. Without scripts, the fields are plain inputs.

import { CATEGORIES, METATAGS } from "./metatags.ts";

/** `search`: search syntax (`-`, `~`, metatags). `tags`: tag input
 * (category prefixes). */
export type Mode = "search" | "tags";

interface Suggestion {
  name: string;
  category: string;
  post_count: number;
  antecedent?: string;
}

interface Item {
  /** What replaces the typed part. */
  text: string;
  /** Add a space after, to start the next word. */
  finished: boolean;
  category?: string;
  count?: number;
  antecedent?: string;
}

/** The part of the word under the cursor that is being completed. */
interface Target {
  /** Where the completed part starts and the word ends. */
  start: number;
  end: number;
  /** What was typed of it. */
  typed: string;
  kind: "tag" | "metatag-name" | "metatag-value";
  /** For metatag values: which metatag. */
  metatag?: string;
}

type Field = HTMLInputElement | HTMLTextAreaElement;

const DEBOUNCE_MS = 120;
const cache = new Map<string, Suggestion[]>();
let nextId = 0;

/** Finds what to complete at `caret` in `value`. */
export function target(value: string, caret: number, mode: Mode): Target | null {
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
      // Lists (`rating:g,s`) complete their last item.
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

async function fetchSuggestions(typed: string, signal: AbortSignal): Promise<Suggestion[]> {
  const key = typed.toLowerCase();
  const cached = cache.get(key);
  if (cached) return cached;
  const response = await fetch(`/tags/autocomplete?q=${encodeURIComponent(typed)}`, {
    signal,
    headers: { Accept: "application/json" },
  });
  if (!response.ok) return [];
  const suggestions = (await response.json()) as Suggestion[];
  if (cache.size >= 500) cache.clear();
  cache.set(key, suggestions);
  return suggestions;
}

function metatagNames(typed: string): Item[] {
  const lower = typed.toLowerCase();
  return Object.keys(METATAGS)
    .filter((name) => name.startsWith(lower))
    .map((name) => ({ text: `${name}:`, finished: false }));
}

function metatagValues(metatag: string, typed: string): Item[] {
  const lower = typed.toLowerCase();
  return (METATAGS[metatag] ?? [])
    .filter((value) => value.startsWith(lower) && value !== lower)
    .map((value) => ({ text: value, finished: true }));
}

class Autocomplete {
  private readonly field: Field;
  private readonly mode: Mode;
  private readonly list: HTMLUListElement;
  private items: Item[] = [];
  private active = -1;
  private current: Target | null = null;
  private timer: number | undefined;
  private request: AbortController | undefined;

  constructor(field: Field, mode: Mode) {
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
      // Keep focus in the field.
      event.preventDefault();
      const option = (event.target as Element).closest("[role=option]");
      const index = option ? Number(option.getAttribute("data-index")) : -1;
      if (index >= 0) this.accept(index);
    });
  }

  private schedule(): void {
    window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.update(), DEBOUNCE_MS);
  }

  private async update(): Promise<void> {
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
    let suggestions: Suggestion[];
    try {
      suggestions = await fetchSuggestions(found.typed, request.signal);
    } catch {
      // Aborted by newer typing, or offline: nothing to suggest.
      return;
    }
    if (request.signal.aborted) return;
    const items: Item[] = suggestions.map((s) => ({
      text: s.name,
      finished: true,
      category: s.category,
      count: s.post_count,
      ...(s.antecedent === undefined ? {} : { antecedent: s.antecedent }),
    }));
    if (this.mode === "search") items.push(...metatagNames(found.typed).slice(0, 3));
    this.show(items);
  }

  private show(items: Item[]): void {
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
          from.textContent = `${item.antecedent} →`;
          option.append(from);
        }
        const name = document.createElement("span");
        name.className = item.category ? `tag tag-${item.category}` : "metatag";
        name.textContent = item.text;
        option.append(name);
        if (item.count !== undefined) {
          const count = document.createElement("span");
          count.className = "count";
          count.textContent = String(item.count);
          option.append(count);
        }
        return option;
      }),
    );
    const open = items.length > 0;
    this.list.hidden = !open;
    this.field.setAttribute("aria-expanded", String(open));
    this.field.removeAttribute("aria-activedescendant");
  }

  private close(): void {
    window.clearTimeout(this.timer);
    this.request?.abort();
    this.show([]);
  }

  private highlight(index: number): void {
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

  private onKey(event: KeyboardEvent): void {
    const open = !this.list.hidden;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        if (!open) return;
        event.preventDefault();
        // Cycles through the options and back to none (-1), the text as
        // typed.
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

  private accept(index: number): void {
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
    // A metatag name continues with its values.
    if (!item.finished) this.schedule();
  }
}

/** Adds autocomplete to every field marked `data-autocomplete`. */
export function attachAll(root: ParentNode = document): void {
  for (const field of root.querySelectorAll<Field>("input[data-autocomplete], textarea[data-autocomplete]")) {
    const mode = field.dataset["autocomplete"] === "search" ? "search" : "tags";
    new Autocomplete(field, mode);
  }
}
