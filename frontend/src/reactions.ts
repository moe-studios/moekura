// Sends the vote and favorite forms in the background and updates the
// counts in place. Without scripts, the forms submit and reload the page.

interface Reactions {
  fav_count: number;
  favorited: boolean;
  score: number;
  vote: number;
}

function update(root: ParentNode, state: Reactions): void {
  const score = root.querySelector(".vote .score");
  if (score) score.textContent = String(state.score);
  const favCount = root.querySelector(".favorite .fav-count");
  if (favCount) favCount.textContent = String(state.fav_count);
  for (const button of root.querySelectorAll<HTMLButtonElement>(".vote button[name=score]")) {
    // Each button votes its way, or takes the vote back when pressed.
    const direction = button.getAttribute("aria-label") === "Vote up" ? 1 : -1;
    const pressed = state.vote === direction;
    button.setAttribute("aria-pressed", String(pressed));
    button.value = String(pressed ? 0 : direction);
  }
  const favorite = root.querySelector<HTMLButtonElement>(".favorite button[name=action]");
  if (favorite) {
    favorite.setAttribute("aria-pressed", String(state.favorited));
    favorite.value = state.favorited ? "remove" : "add";
  }
}

export function enhanceReactions(root: Document = document): void {
  for (const form of root.querySelectorAll<HTMLFormElement>("form[data-reaction]")) {
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const submitter = event.submitter instanceof HTMLButtonElement ? event.submitter : null;
      const body = new URLSearchParams();
      for (const [name, value] of new FormData(form, submitter)) {
        if (typeof value === "string") body.append(name, value);
      }
      void fetch(form.action, {
        method: "POST",
        body,
        headers: { Accept: "application/json" },
      })
        .then(async (response) => {
          if (!response.ok) throw new Error(String(response.status));
          update(root, (await response.json()) as Reactions);
        })
        // Fall back to a normal submission, which shows any error.
        .catch(() => form.submit());
    });
  }
}
