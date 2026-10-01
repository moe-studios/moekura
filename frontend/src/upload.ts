// Files from the clipboard and drag-and-drop use the same native multipart
// field as the picker. Selecting a file never submits the form.

import { t } from "./i18n.ts";
export function enableUpload(root: Document = document): void {
  const form = root.querySelector<HTMLFormElement>("form[data-upload]");
  const input = form?.querySelector<HTMLInputElement>('input[type="file"]');
  const zone = form?.querySelector<HTMLElement>("[data-upload-drop-zone]");
  const status = form?.querySelector<HTMLElement>("[data-upload-status]");
  if (!form || !input || !zone || !status || typeof DataTransfer === "undefined") return;

  zone.classList.add("enhanced");
  const hint = form.querySelector<HTMLElement>("[data-upload-hint]");
  if (hint) hint.hidden = false;

  const select = (files: FileList): void => {
    if (files.length !== 1) {
      status.textContent = t("upload-one-file", "Choose one file at a time. Your current selection has not changed.");
      return;
    }
    const transfer = new DataTransfer();
    transfer.items.add(files[0]!);
    input.files = transfer.files;
    input.dispatchEvent(new Event("change", { bubbles: true }));
  };
  input.addEventListener("change", () => {
    status.textContent = input.files?.[0] ? t("upload-selected", "Selected: {$name}", { name: input.files[0].name }) : "";
  });

  root.addEventListener("paste", (event) => {
    const files = event.clipboardData?.files;
    // Leave ordinary text and URL pastes to their focused field.
    if (!files?.length) return;
    event.preventDefault();
    select(files);
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
    if (event.dataTransfer?.files.length) select(event.dataTransfer.files);
  });
}
