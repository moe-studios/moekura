// "Fetch source data" on an uploaded file's post form looks the source up
// again and shows what it says, without sending the form.

export function enableSourceData(root: Document = document): void {
  const button = root.querySelector<HTMLButtonElement>("button[data-source-fetch]");
  const form = button?.form;
  const input = form?.querySelector<HTMLInputElement>('input[name="source"]');
  const panel = form?.querySelector<HTMLElement>("[data-source-panel]");
  if (!button || !input || !panel) return;
  button.addEventListener("click", async (event) => {
    event.preventDefault();
    const query = new URLSearchParams({ url: input.value.trim(), refresh: "1" });
    button.disabled = true;
    panel.setAttribute("aria-busy", "true");
    try {
      const response = await fetch(`/uploads/source-data?${query.toString()}`, { credentials: "same-origin" });
      if (response.ok) panel.innerHTML = await response.text();
    } catch {
      // Offline: the panel stays as it was.
    } finally {
      button.disabled = false;
      panel.removeAttribute("aria-busy");
    }
  });
}
