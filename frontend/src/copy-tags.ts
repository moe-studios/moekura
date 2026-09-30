// "Copy tags" on the post edit form: clicking a related post adds its
// tags to the tags box rather than saving the form. Without scripts, each
// click adds them and saves.

import { withTag } from "./suggestions.ts";

/** `tags` with every word of `copied` added at the end, unless there. */
export function withTags(tags: string, copied: string): string {
  return copied
    .split(/\s+/)
    .filter((word) => word !== "")
    .reduce((all, word) => withTag(all, word), tags);
}

export function enableCopyTags(root: Document = document): void {
  const box = root.querySelector<HTMLElement>("[data-copy-tags]");
  const field = box?.closest("form")?.querySelector<HTMLTextAreaElement>("textarea[name=tags]");
  if (!box || !field) return;
  const hint = box.querySelector("[data-copy-tags-hint]");
  if (hint) hint.textContent = "Clicking one adds that post's tags to the form; save to keep them.";

  box.addEventListener("click", (event) => {
    const button = (event.target as Element).closest<HTMLButtonElement>("button[data-tags]");
    if (!button) return;
    event.preventDefault();
    field.value = withTags(field.value, button.dataset["tags"] ?? "");
    button.classList.add("chosen");
    button.disabled = true;
  });
}
