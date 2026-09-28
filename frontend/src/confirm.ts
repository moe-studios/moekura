// Asks before submitting a form that can't easily be undone. The form, or
// the button that submits it, carries the question in data-confirm.
// Without scripts the form submits straight away.

/** The question for submitting `form` with `submitter`, if any. Pure, for tests. */
export function questionFor(form: Element, submitter: Element | null): string | null {
  return submitter?.getAttribute("data-confirm") ?? form.getAttribute("data-confirm");
}

export function enableConfirm(): void {
  // Capturing, so this runs before any other submit handler.
  document.addEventListener(
    "submit",
    (event) => {
      const question = questionFor(event.target as Element, event.submitter);
      if (question !== null && !window.confirm(question)) {
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    },
    true,
  );
}
