// Forms that take a file besides the upload page's (replacing a post's
// file, searching by image) send it in pieces first, when they carry
// data-transfer-files, saying what the file is for: then no request has
// to carry the whole file (see transfer.ts). The form then names the file
// instead of sending it. Without scripts, the form sends the file itself.

import { t } from "./i18n.ts";
import { TRANSFERS, TransferError, sendFile } from "./transfer.ts";

/** What forms may send files for; the server checks it too. */
const PURPOSES = ["upload", "replace", "search"];

export function enableTransferForms(root: Document = document): void {
  for (const form of root.querySelectorAll<HTMLFormElement>("form[data-transfer-files]")) enable(root, form);
}

function enable(root: Document, form: HTMLFormElement): void {
  // Attributes are read through Element itself: a form's own properties
  // can be shadowed by fields named after them.
  const attribute = (name: string): string | null => Element.prototype.getAttribute.call(form, name);
  const purpose = attribute("data-transfer-files") ?? "";
  const input = form.querySelector<HTMLInputElement>('input[type="file"][name="file"]');
  if (!PURPOSES.includes(purpose) || !input) return;
  const piece = Number(attribute("data-transfer-piece")) || 50 * 1024 * 1024;
  const status = form.querySelector<HTMLElement>("[data-transfer-status]");
  const error = form.querySelector<HTMLElement>("[data-transfer-error]");
  const here = root.location?.href ?? "";
  let sending = false;

  const busy = (on: boolean): void => {
    sending = on;
    for (const button of form.querySelectorAll<HTMLButtonElement>("button[type=submit], button:not([type])")) button.disabled = on;
  };
  // Files named by an earlier send are used up.
  const forget = (): void => {
    for (const field of form.querySelectorAll("input[data-transfer-token]")) field.remove();
  };

  const send = async (files: File[]): Promise<void> => {
    busy(true);
    if (error) error.hidden = true;
    const total = files.reduce((sum, file) => sum + file.size, 0) || 1;
    let before = 0;
    const show = (sent: number): void => {
      const percent = Math.min(100, Math.floor(((before + sent) * 100) / total));
      if (status) status.textContent = t("transfer-progress", "Sending {$name}… {$percent}%", { name: files[0]!.name, percent });
    };
    const retrying = (): void => {
      if (status) status.textContent = t("upload-retrying", "The connection faltered; trying again…");
    };
    const tokens: string[] = [];
    try {
      for (const file of files) {
        show(0);
        tokens.push(await sendFile(file, file.name, { maxPiece: piece, base: here, purpose, progress: show, retrying }));
        before += file.size;
      }
    } catch (failure) {
      for (const token of tokens) {
        const url = new URL(`${TRANSFERS}/${token}`, here);
        void fetch(url, { method: "DELETE", headers: { "Tus-Resumable": "1.0.0" }, credentials: "same-origin" }).catch(() => undefined);
      }
      if (error) {
        error.textContent = (failure instanceof TransferError && failure.message) || t("transfer-failed", "Sending the file failed. Please try again.");
        error.hidden = false;
      }
      if (status) status.textContent = "";
      busy(false);
      return;
    }
    // The form names the files instead of sending them again. Submitting
    // it directly skips its submit handlers, which ran already (a
    // question asked before it was sent, for one).
    input.value = "";
    forget();
    for (const token of tokens) {
      const field = root.createElement("input");
      field.type = "hidden";
      field.name = "transfer";
      field.value = token;
      field.setAttribute("data-transfer-token", "");
      form.append(field);
    }
    HTMLFormElement.prototype.submit.call(form);
  };

  form.addEventListener("submit", (event) => {
    const files = Array.from(input.files ?? []);
    // Without a file (a link instead), the form is sent as it is.
    if (files.length === 0 || event.defaultPrevented) return;
    event.preventDefault();
    if (!sending) void send(files);
  });
  // Coming back to the page (the back button) finds the form usable.
  root.defaultView?.addEventListener("pageshow", () => {
    forget();
    busy(false);
  });
}
