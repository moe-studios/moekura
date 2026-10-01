// Tag scripts: on search results, those who can edit posts type a script
// like `tag_a -tag_b rating:s pool:12`, then clicking a post applies it
// (through the API) instead of opening it. Metatags work as in the tag
// box; the server reads them. Each change is in the post's history as
// usual. Without scripts the panel stays hidden.

import { t } from "./i18n.ts";

import { NEGATABLE } from "./metatags.ts";

export interface Script {
  add: string[];
  remove: string[];
  rating: string | null;
}

const RATINGS: Readonly<Record<string, string>> = {
  g: "g",
  general: "g",
  s: "s",
  sensitive: "s",
  q: "q",
  questionable: "q",
  e: "e",
  explicit: "e",
};

/** Parses a script; throws with a message for a bad rating. */
export function parseScript(text: string): Script {
  const script: Script = { add: [], remove: [], rating: null };
  for (const word of text.trim().split(/\s+/)) {
    if (word === "") continue;
    const lower = word.toLowerCase();
    if (lower.startsWith("rating:")) {
      const rating = RATINGS[lower.slice("rating:".length)];
      if (!rating) throw new Error(t("tag-script-rating", "Unknown rating in “{$word}”.", { word }));
      script.rating = rating;
    } else if (word.startsWith("-") && word.length > 1 && !isNegatedMetatag(lower)) {
      script.remove.push(word.slice(1));
    } else {
      script.add.push(word);
    }
  }
  return script;
}

/** Whether `word` (lower case) undoes a metatag, like `-pool:12` or
 * `-fav`, rather than taking a tag off. */
function isNegatedMetatag(word: string): boolean {
  const name = word.slice(1).split(":")[0] ?? "";
  return NEGATABLE.includes(name) && (word.includes(":") || name === "fav" || name === "parent");
}

/** The post number a card links to. */
export function postId(href: string): string | null {
  return /\/posts\/(\d+)/.exec(href)?.[1] ?? null;
}

export function enableTagScript(root: Document = document): void {
  const panel = root.querySelector<HTMLElement>("[data-tag-script]");
  const input = panel?.querySelector<HTMLInputElement>("input");
  const toggle = panel?.querySelector<HTMLInputElement>("input[type=checkbox]");
  const status = panel?.querySelector<HTMLElement>("[data-tag-script-status]");
  const text = panel?.querySelector<HTMLInputElement>("input[type=text]");
  if (!panel || !input || !toggle || !status || !text) return;
  panel.hidden = false;

  const say = (message: string) => {
    status.textContent = message;
  };

  root.addEventListener(
    "click",
    (event) => {
      if (!toggle.checked) return;
      const card = (event.target as Element).closest<HTMLAnchorElement>(".post-grid a.card");
      if (!card) return;
      event.preventDefault();
      const id = postId(card.getAttribute("href") ?? "");
      if (!id) return;
      let script: Script;
      try {
        script = parseScript(text.value);
      } catch (error) {
        say((error as Error).message);
        return;
      }
      if (script.add.length === 0 && script.remove.length === 0 && script.rating === null) {
        say(t("tag-script-empty", "Type a script first."));
        return;
      }
      const body: Record<string, unknown> = { add_tags: script.add, remove_tags: script.remove };
      if (script.rating) body["rating"] = script.rating;
      card.classList.remove("script-ok", "script-failed");
      card.classList.add("script-busy");
      void fetch(`/api/v1/posts/${id}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        credentials: "same-origin",
        body: JSON.stringify(body),
      })
        .then(async (response) => {
          card.classList.remove("script-busy");
          if (response.ok) {
            card.classList.add("script-ok");
            say(t("tag-script-changed", "Post #{$id} changed.", { id }));
          } else {
            card.classList.add("script-failed");
            const error = (await response.json().catch(() => null)) as { error?: { message?: string } } | null;
            say(
              t("tag-script-error", "Post #{$id}: {$error}", {
                id,
                error: error?.error?.message ?? t("error-status", "Error {$status}", { status: response.status }),
              }),
            );
          }
        })
        .catch(() => {
          card.classList.remove("script-busy");
          card.classList.add("script-failed");
          say(t("tag-script-failed", "Post #{$id} couldn't be changed.", { id }));
        });
    },
    true,
  );
}
