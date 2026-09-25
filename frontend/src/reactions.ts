// Sends the vote and favorite forms in the background and updates the
// counts in place. Without scripts, the forms submit and reload the page.

// A post's reactions, or a comment's (score and vote only).
interface Reactions {
  fav_count?: number;
  favorited?: boolean;
  score: number;
  vote: number;
}

function update(root: ParentNode, state: Reactions): void {
  const score = root.querySelector(".vote .score");
  if (score) score.textContent = String(state.score);
  const favCount = root.querySelector(".favorite .fav-count");
  if (favCount && state.fav_count !== undefined) favCount.textContent = String(state.fav_count);
  for (const button of root.querySelectorAll<HTMLButtonElement>(".vote button[name=score]")) {
    // Each button votes its way, or takes the vote back when pressed.
    const direction = button.getAttribute("aria-label") === "Vote up" ? 1 : -1;
    const pressed = state.vote === direction;
    button.setAttribute("aria-pressed", String(pressed));
    button.value = String(pressed ? 0 : direction);
  }
  const favorite = root.querySelector<HTMLButtonElement>(".favorite button[name=favorite]");
  if (favorite && state.favorited !== undefined) {
    favorite.setAttribute("aria-pressed", String(state.favorited));
    favorite.value = state.favorited ? "remove" : "add";
  }
}

export function enhanceReactions(root: Document = document): void {
  for (const form of root.querySelectorAll<HTMLFormElement>("form[data-reaction]")) {
    form.addEventListener("submit", (event) => {
      // Set when falling back to a normal submission.
      if (form.dataset["plain"]) return;
      event.preventDefault();
      const submitter = event.submitter instanceof HTMLButtonElement ? event.submitter : null;
      const body = new URLSearchParams();
      for (const [name, value] of new FormData(form, submitter)) {
        if (typeof value === "string") body.append(name, value);
      }
      // getAttribute: a control named "action" would shadow form.action.
      void fetch(form.getAttribute("action") ?? "", {
        method: "POST",
        body,
        headers: { Accept: "application/json" },
      })
        .then(async (response) => {
          if (!response.ok) throw new Error(String(response.status));
          // Only the post's or comment's own counts: a post page has both.
          const scope = form.closest(".comment") ?? form.closest(".post-info") ?? root;
          update(scope, (await response.json()) as Reactions);
        })
        // Fall back to a normal submission, which shows any error.
        .catch(() => {
          form.dataset["plain"] = "1";
          form.requestSubmit(submitter);
        });
    });
  }
}
