// Short messages from scripts, shown at the bottom of the page for a few
// seconds and read out by screen readers. For problems that don't need a
// page of their own, like a vote sent too quickly.

const SHOW_FOR_MS = 6000;

/** Adds the (empty) message area. Screen readers only announce changes
 * to a live region that was already on the page. */
export function enableToasts(): void {
  const list = document.createElement("ul");
  list.id = "toasts";
  list.className = "toasts";
  list.setAttribute("role", "status");
  document.body.append(list);
}

export function toast(message: string): void {
  const region = document.getElementById("toasts");
  if (!region) {
    window.alert(message);
    return;
  }
  const item = document.createElement("li");
  item.className = "toast";
  item.textContent = message;
  region.append(item);
  window.setTimeout(() => item.remove(), SHOW_FOR_MS);
}
