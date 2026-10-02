// The tagger's suggestions on the post edit form and on an upload's post
// form: clicking one adds the tag to the tags box (or picks the rating)
// rather than sending the form, so several can be taken first. Without
// scripts, each click sends the form: the edit form saves, the upload
// form comes back with the suggestion in it. On the upload form the
// suggestions may not be there yet; they're fetched until they are.

import { t } from "./i18n.ts";

/** How often the upload form asks for suggestions still to come. */
const POLL_MS = 3000;
/** How long it keeps asking. */
const POLL_FOR_MS = 5 * 60 * 1000;

/** `tags` with `tag` added at the end, unless it is there already. */
export function withTag(tags: string, tag: string): string {
  const words = tags.split(/\s+/).filter((word) => word !== "");
  if (words.includes(tag)) return tags;
  const kept = tags.trimEnd();
  return kept === "" ? `${tag} ` : `${kept} ${tag} `;
}

/** Whether `tags` (a tags box) has `tag`, with or without a category prefix. */
export function hasTag(tags: string, tag: string): boolean {
  return tags
    .split(/\s+/)
    .some((word) => word.toLowerCase() === tag || word.toLowerCase().endsWith(`:${tag}`));
}

export function enableSuggestions(root: Document = document): void {
  const box = root.querySelector<HTMLElement>("[data-suggestions]");
  const form = box?.closest("form");
  const field = form?.querySelector<HTMLTextAreaElement>("textarea[name=tags]");
  if (!box || !form || !field) return;
  const upload = box.dataset["suggestions"] === "upload";

  // Suggestions already in the form are shown as taken.
  const prepare = () => {
    const hint = box.querySelector("[data-suggestions-hint]");
    if (hint && !upload) hint.textContent = t("suggestions-hint", "Clicking one adds it to the form; save to keep it.");
    const rating = form.querySelector<HTMLInputElement>("input[name=rating]:checked")?.value;
    for (const button of box.querySelectorAll<HTMLButtonElement>("button[name]")) {
      const taken =
        (button.name === "add" && hasTag(field.value, button.value)) ||
        (button.name === "suggested_rating" && button.value === rating);
      if (taken) {
        button.classList.add("chosen");
        button.disabled = true;
      }
    }
    for (const check of box.querySelectorAll<HTMLElement>("[data-suggestions-check]")) check.hidden = true;
  };
  prepare();

  box.addEventListener("click", (event) => {
    const button = (event.target as Element).closest<HTMLButtonElement>("button[name]");
    if (!button) return;
    event.preventDefault();
    if (button.name === "add") {
      field.value = withTag(field.value, button.value);
    } else if (button.name === "suggested_rating") {
      for (const radio of form.querySelectorAll<HTMLInputElement>("input[name=rating]")) {
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
      // Offline for a moment: ask again.
    }
    if (Date.now() - started < POLL_FOR_MS) window.setTimeout(() => void poll(), POLL_MS);
  };
  window.setTimeout(() => void poll(), POLL_MS);
}
