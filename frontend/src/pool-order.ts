// Reorders a pool's posts by dragging their thumbnails on the pool editor,
// rewriting the list of post numbers the form sends. Without scripts, the
// numbers are edited by hand.

/** The ids in `text` with `moved` put before `before` (or at the end). */
export function reorder(ids: readonly string[], moved: string, before: string | null): string[] {
  const rest = ids.filter((id) => id !== moved);
  const at = before === null ? -1 : rest.indexOf(before);
  if (at < 0) return [...rest, moved];
  return [...rest.slice(0, at), moved, ...rest.slice(at)];
}

export function enablePoolOrder(root: Document = document): void {
  const list = root.querySelector<HTMLOListElement>("[data-pool-order]");
  const field = root.querySelector<HTMLTextAreaElement>("[data-pool-posts]");
  if (!list || !field) return;
  let dragged: HTMLLIElement | null = null;

  for (const item of list.querySelectorAll<HTMLLIElement>("li[data-id]")) {
    item.draggable = true;
    item.addEventListener("dragstart", (event) => {
      dragged = item;
      item.classList.add("dragging");
      event.dataTransfer?.setData("text/plain", item.dataset["id"] ?? "");
    });
    item.addEventListener("dragend", () => {
      item.classList.remove("dragging");
      dragged = null;
    });
    // Following links while dragging would lose the edit.
    item.querySelector("a")?.addEventListener("click", (event) => event.preventDefault());
  }

  list.addEventListener("dragover", (event) => {
    if (!dragged) return;
    event.preventDefault();
    const target = (event.target as Element).closest<HTMLLIElement>("li[data-id]");
    if (!target || target === dragged) return;
    // Before the target when over its first half, after it otherwise.
    const box = target.getBoundingClientRect();
    const after = event.clientX > box.left + box.width / 2;
    list.insertBefore(dragged, after ? target.nextSibling : target);
  });

  list.addEventListener("drop", (event) => {
    event.preventDefault();
    if (!dragged) return;
    const moved = dragged.dataset["id"] ?? "";
    const next = dragged.nextElementSibling as HTMLLIElement | null;
    // Posts beyond the thumbnails shown keep their place after them.
    const ids = field.value.split(/[\s,]+/).filter((id) => id !== "").map((id) => id.replace(/^#/, ""));
    field.value = reorder(ids, moved, next?.dataset["id"] ?? null).join(" ");
  });
}
