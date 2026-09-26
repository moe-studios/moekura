// The tagger's suggestions on the post edit form: clicking one adds the
// tag to the tags box (or picks the rating) rather than saving the form,
// so several can be taken before saving. Without scripts, each click
// saves.

/** `tags` with `tag` added at the end, unless it is there already. */
export function withTag(tags: string, tag: string): string {
  const words = tags.split(/\s+/).filter((word) => word !== "");
  if (words.includes(tag)) return tags;
  const kept = tags.trimEnd();
  return kept === "" ? `${tag} ` : `${kept} ${tag} `;
}

export function enableSuggestions(root: Document = document): void {
  const box = root.querySelector<HTMLElement>("[data-suggestions]");
  const form = box?.closest("form");
  const field = form?.querySelector<HTMLTextAreaElement>("textarea[name=tags]");
  if (!box || !form || !field) return;
  const hint = box.querySelector("[data-suggestions-hint]");
  if (hint) hint.textContent = "Clicking one adds it to the form; save to keep it.";

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
}
