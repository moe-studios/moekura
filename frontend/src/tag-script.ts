// Tag scripts: on search results, those who can edit posts type a script
// like `tag_a -tag_b rating:s`, then clicking a post applies it (through
// the API) instead of opening it. Each change is in the post's history as
// usual. Without scripts the panel stays hidden.

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
      if (!rating) throw new Error(`Unknown rating in “${word}”.`);
      script.rating = rating;
    } else if (word.startsWith("-") && word.length > 1) {
      script.remove.push(word.slice(1));
    } else {
      script.add.push(word);
    }
  }
  return script;
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
        say("Type a script first.");
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
            say(`Post #${id} changed.`);
          } else {
            card.classList.add("script-failed");
            const error = (await response.json().catch(() => null)) as { error?: { message?: string } } | null;
            say(`Post #${id}: ${error?.error?.message ?? `error ${response.status}`}`);
          }
        })
        .catch(() => {
          card.classList.remove("script-busy");
          card.classList.add("script-failed");
          say(`Post #${id} couldn't be changed.`);
        });
    },
    true,
  );
}
