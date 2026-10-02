// The upload form sends files as soon as they're chosen, as Danbooru's
// does: from the picker, the clipboard or a drop, all through the same
// native multipart field. A link is typed and sent with the button.

import { t } from "./i18n.ts";

export function enableUpload(root: Document = document): void {
  const form = root.querySelector<HTMLFormElement>("form[data-upload]");
  const input = form?.querySelector<HTMLInputElement>('input[type="file"]');
  const zone = form?.querySelector<HTMLElement>("[data-upload-drop-zone]");
  const status = form?.querySelector<HTMLElement>("[data-upload-status]");
  if (!form || !input || !zone || !status) return;
  const max = Number(input.dataset["max"] ?? "1") || 1;
  let sending = false;

  const send = (files: FileList): void => {
    if (sending || files.length === 0) return;
    if (files.length > max) {
      status.textContent = t("upload-too-many", "Choose at most {$max} files at once.", { max });
      return;
    }
    if (files !== input.files) {
      const transfer = new DataTransfer();
      for (const file of files) transfer.items.add(file);
      input.files = transfer.files;
    }
    status.textContent = files.length === 1
      ? t("upload-sending-one", "Uploading {$name}…", { name: files[0]!.name })
      : t("upload-sending-many", "Uploading {$count} files…", { count: files.length });
    sending = true;
    zone.classList.add("sending");
    form.requestSubmit();
  };
  input.addEventListener("change", () => {
    if (input.files) send(input.files);
  });
  form.addEventListener("submit", () => {
    sending = true;
    for (const button of form.querySelectorAll<HTMLButtonElement>("button[type=submit]")) button.disabled = true;
  });
  // Coming back to the page (the back button) finds the form usable.
  root.defaultView?.addEventListener("pageshow", () => {
    sending = false;
    zone.classList.remove("sending");
    for (const button of form.querySelectorAll<HTMLButtonElement>("button[type=submit]")) button.disabled = false;
  });

  if (typeof DataTransfer === "undefined") return;
  zone.classList.add("enhanced");
  const hint = form.querySelector<HTMLElement>("[data-upload-hint]");
  if (hint) hint.hidden = false;

  root.addEventListener("paste", (event) => {
    const files = event.clipboardData?.files;
    // Leave ordinary text and URL pastes to their focused field.
    if (!files?.length) return;
    event.preventDefault();
    send(files);
  });

  const hasFiles = (event: DragEvent): boolean =>
    event.dataTransfer?.types.includes("Files") ?? false;
  let depth = 0;
  form.addEventListener("dragenter", (event) => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    depth++;
    zone.classList.add("dragging");
  });
  form.addEventListener("dragover", (event) => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });
  form.addEventListener("dragleave", () => {
    depth = Math.max(0, depth - 1);
    if (!depth) zone.classList.remove("dragging");
  });
  form.addEventListener("drop", (event) => {
    depth = 0;
    zone.classList.remove("dragging");
    if (!hasFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer?.files.length) send(event.dataTransfer.files);
  });
}
