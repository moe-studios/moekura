// Suggests the artist tag on the upload form: when the link to upload
// from, or the source, is a page of an artist with an artist entry, their
// tag is offered, and a click adds it to the tags box.

import { withTag } from "./suggestions.ts";

interface Found {
  id: number;
  name: string;
  url: string;
}

const DEBOUNCE_MS = 400;

/** The first of `values` that looks like a web address. */
export function firstUrl(values: string[]): string | null {
  for (const value of values) {
    const trimmed = value.trim();
    if (/^https?:\/\/\S+$/i.test(trimmed)) return trimmed;
  }
  return null;
}

export function enableArtistFinder(root: Document = document): void {
  const box = root.querySelector<HTMLElement>("[data-artist-finder]");
  const form = box?.closest("form");
  const tags = form?.querySelector<HTMLTextAreaElement>("textarea[name=tags]");
  if (!box || !form || !tags) return;
  const inputs = (box.dataset["artistFinder"] ?? "")
    .split(/\s+/)
    .map((name) => form.querySelector<HTMLInputElement>(`input[name="${name}"]`))
    .filter((input): input is HTMLInputElement => input !== null);
  let timer: number | undefined;
  let request: AbortController | undefined;
  let last = "";

  const show = (found: Found[]) => {
    if (found.length === 0) {
      box.hidden = true;
      box.replaceChildren();
      return;
    }
    const label = document.createElement("span");
    label.className = "hint";
    label.textContent = found.length === 1 ? "Artist: " : "Artists: ";
    const buttons = found.map((artist) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "tag tag-artist link";
      button.dataset["tag"] = artist.name;
      button.textContent = artist.name;
      button.title = "Add to the tags";
      return button;
    });
    box.replaceChildren(label, ...buttons);
    box.hidden = false;
  };

  const update = async () => {
    const url = firstUrl(inputs.map((input) => input.value));
    if (url === null) {
      last = "";
      show([]);
      return;
    }
    if (url === last) return;
    last = url;
    request?.abort();
    request = new AbortController();
    try {
      const response = await fetch(`/artists/finder?${new URLSearchParams({ url }).toString()}`, {
        signal: request.signal,
        headers: { Accept: "application/json" },
      });
      if (!response.ok) return;
      show((await response.json()) as Found[]);
    } catch {
      // Aborted by newer typing, or offline.
    }
  };
  const schedule = () => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void update(), DEBOUNCE_MS);
  };
  for (const input of inputs) input.addEventListener("input", schedule);
  box.addEventListener("click", (event) => {
    const tag = (event.target as Element).closest<HTMLButtonElement>("button[data-tag]")?.dataset["tag"];
    if (!tag) return;
    tags.value = withTag(tags.value, tag);
    tags.dispatchEvent(new Event("input", { bubbles: true }));
  });
  void update();
}
