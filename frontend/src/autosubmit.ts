// Submits a form marked data-autosubmit as soon as one of its selects
// changes, so its submit button can hide. Without scripts the button
// submits it.

export function enableAutosubmit(): void {
  document.addEventListener("change", (event) => {
    const target = event.target;
    if (!(target instanceof HTMLSelectElement)) return;
    target.closest<HTMLFormElement>("form[data-autosubmit]")?.requestSubmit();
  });
}
