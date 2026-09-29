// A "tick every one" checkbox: `data-select-all="form-id"` ticks and
// unticks every checkbox that belongs to that form through its `form`
// attribute. Without scripts, each is ticked by hand.

/** The checkboxes `master` controls. */
function controlled(master: HTMLInputElement): HTMLInputElement[] {
  const form = master.dataset.selectAll;
  if (!form) return [];
  return Array.from(document.querySelectorAll<HTMLInputElement>(`input[type=checkbox][form="${CSS.escape(form)}"]`));
}

export function enableSelectAll(): void {
  for (const master of document.querySelectorAll<HTMLInputElement>("input[data-select-all]")) {
    master.addEventListener("change", () => {
      for (const box of controlled(master)) box.checked = master.checked;
    });
  }
}
