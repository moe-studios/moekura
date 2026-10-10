// The related tags panel beside the tags box of the upload and edit
// forms: tags often used with those in the box (or with the one under
// the caret), your recent and frequent tags, translations of words that
// are wiki other names, and the chosen tag's wiki links. Clicking a tag
// adds it to the box, or takes it out if it's there. The panel follows
// the box as it changes. Without scripts, the panel is a link to the
// related tags page.

import { t } from "./i18n.ts";

import { withTag } from "./suggestions.ts";

interface RelatedTag {
  name: string;
  category: string;
  post_count: number;
  selected: boolean;
  from?: string;
}

interface Group {
  kind: string;
  title: string;
  tags: RelatedTag[];
}

const DEBOUNCE_MS = 400;

/** The plain tag under the caret, if any (not a metatag or `-tag`). */
export function chosenTag(value: string, caret: number): string | null {
  let start = caret;
  while (start > 0 && !/\s/.test(value.charAt(start - 1))) start--;
  let end = caret;
  while (end < value.length && !/\s/.test(value.charAt(end))) end++;
  const word = value.slice(start, end);
  if (word === "" || word.startsWith("-") || word.includes(":") || word.includes("*")) return null;
  return word.toLowerCase();
}

/** `tags` without every `tag` word. */
export function withoutTag(tags: string, tag: string): string {
  const words = tags.split(/\s+/).filter((word) => word !== "" && word.toLowerCase() !== tag);
  return words.length === 0 ? "" : `${words.join(" ")} `;
}

function hasTag(tags: string, tag: string): boolean {
  return tags.split(/\s+/).some((word) => word.toLowerCase() === tag);
}

/** The link being uploaded, or else the source, from the field's form. */
function sourceOf(field: HTMLTextAreaElement): string | null {
  const form = field.form;
  if (!form) return null;
  for (const name of ["url", "source"]) {
    const input = form.querySelector<HTMLInputElement>(`input[name="${name}"]`);
    const value = input?.value.trim() ?? "";
    if (/^https?:\/\/\S+$/i.test(value)) return value;
  }
  return null;
}

/** Tags a related list shows before "Show all". Kept in step with main.css. */
const RELATED_SHOWN = 10;

export function enableRelatedTags(root: Document = document): void {
  for (const panel of root.querySelectorAll<HTMLElement>("[data-related-tags]")) {
    const field = root.getElementById(panel.dataset["relatedTags"] ?? "");
    if (field instanceof HTMLTextAreaElement) attach(panel, field);
  }
}

function attach(panel: HTMLElement, field: HTMLTextAreaElement): void {
  const list = document.createElement("div");
  list.className = "related-groups";
  list.setAttribute("aria-live", "polite");
  panel.append(list);
  let timer: number | undefined;
  let request: AbortController | undefined;
  let lastUrl = "";

  const render = (groups: Group[]) => {
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
          button.setAttribute("aria-pressed", String(hasTag(field.value, tag.name)));
          button.textContent = tag.name;
          button.title = tag.from
            ? `${tag.from} → ${tag.name}`
            : t("related-posts", "{$count} posts", { count: tag.post_count });
          item.append(button);
          items.append(item);
        }
        section.append(title, items);
        // Long lists show their first rows, with a button for the rest.
        if (group.tags.length > RELATED_SHOWN) {
          section.classList.add("collapsed");
          const more = document.createElement("button");
          more.type = "button";
          more.className = "link related-more";
          more.textContent = t("related-more", "Show all {$count}", { count: group.tags.length });
          more.addEventListener("click", () => {
            section.classList.remove("collapsed");
            more.remove();
            items.querySelector<HTMLButtonElement>(`li:nth-child(${RELATED_SHOWN + 1}) button`)?.focus();
          });
          section.append(more);
        }
        return section;
      }),
    );
  };

  const update = async () => {
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
      const panelData = (await response.json()) as { groups: Group[] };
      render(panelData.groups);
    } catch {
      // Aborted by newer typing, or offline: keep what's shown.
    }
  };
  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void update(), DEBOUNCE_MS);
  };

  field.addEventListener("input", schedule);
  for (const input of field.form?.querySelectorAll<HTMLInputElement>('input[name="url"], input[name="source"]') ?? []) {
    input.addEventListener("change", schedule);
  }
  field.addEventListener("click", schedule);
  field.addEventListener("keyup", (event) => {
    if (event.key.startsWith("Arrow") || event.key === "Home" || event.key === "End") schedule();
  });
  list.addEventListener("click", (event) => {
    const button = (event.target as Element).closest<HTMLButtonElement>("button[data-tag]");
    const tag = button?.dataset["tag"];
    if (!button || !tag) return;
    const present = hasTag(field.value, tag);
    field.value = present ? withoutTag(field.value, tag) : withTag(field.value, tag);
    for (const other of list.querySelectorAll<HTMLButtonElement>("button[data-tag]")) {
      if (other.dataset["tag"] === tag) other.setAttribute("aria-pressed", String(!present));
    }
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
  void update();
}
