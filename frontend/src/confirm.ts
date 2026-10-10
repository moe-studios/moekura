// Asks before submitting a form that can't easily be undone. The form, or
// the button that submits it, carries the question in data-confirm.
// Without scripts the form submits straight away.

/** A form field, as far as a question needs one: a list or an input. */
type Field = Element & { value?: string; selectedOptions?: ArrayLike<{ text: string }> };

/** The question for submitting `form` with `submitter`, if any, with each
 * `%name%` replaced by what the form's field `name` holds: the chosen
 * option's text for a list, else the value. Pure, for tests. */
export function questionFor(form: Element, submitter: Element | null): string | null {
  const question = submitter?.getAttribute("data-confirm") ?? form.getAttribute("data-confirm");
  if (question === null) return null;
  return question.replace(/%([\w-]+)%/g, (whole, name: string) => {
    const field: Field | null = form.querySelector(`[name="${name}"]`);
    if (!field) return whole;
    const option = field.selectedOptions?.[0];
    return (option ? option.text : (field.value ?? "")).trim();
  });
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
