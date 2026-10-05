// The upload form sends files as soon as they're chosen, as Danbooru's
// does: from the picker, the clipboard or a drop anywhere on the page, all
// through the same native multipart field. A link is sent as soon as it's
// pasted anywhere on the page, or when the page was opened with one (the
// bookmarklet); a typed one is sent with the button.

import { t } from "./i18n.ts";

/** Where the upload form sends what it's given. */
const UPLOADS = "/uploads";
/** The upload page, the only one that sends a link given with it. */
const UPLOAD_PAGE = "/uploads/new";

/**
 * Whether a form with these `action` and `method` attributes, on the page
 * at `page`, is the upload form: one posting to this site's `/uploads`.
 * Only that form is sent by itself, so markup slipped into some other
 * page can't have a form of its own sent as the viewer.
 */
export function isUploadForm(action: string | null, method: string | null, page: string): boolean {
  if (action === null || method?.toLowerCase() !== "post") return false;
  try {
    const target = new URL(action, page);
    return target.origin === new URL(page).origin && target.pathname === UPLOADS && target.search === "";
  } catch {
    return false;
  }
}

/** Whether the upload form on the page at `page` sends its link at once. */
export function sendsNow(sendNow: string | null, page: string): boolean {
  try {
    return sendNow !== null && new URL(page).pathname === UPLOAD_PAGE;
  } catch {
    return false;
  }
}

/** `text` as a web link, if it is one (or is one once `https://` is added). */
export function asLink(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed === "" || /\s/.test(trimmed)) return null;
  for (const candidate of [trimmed, `https://${trimmed}`]) {
    try {
      const url = new URL(candidate);
      // A bare word isn't a host: it needs a dot (or to be localhost).
      if ((url.protocol === "http:" || url.protocol === "https:") && /[.:]|^localhost$/.test(url.hostname)) {
        return url.href;
      }
    } catch {
      // Not a link; try the next form.
    }
    // A scheme other than the web's isn't fixed by adding one.
    if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return null;
  }
  return null;
}

export function enableUpload(root: Document = document): void {
  // A page has one upload form; another one is markup slipped in, and
  // then neither is trusted.
  const forms = root.querySelectorAll<HTMLFormElement>("form[data-upload]");
  const form = forms.length === 1 ? forms[0] : undefined;
  if (!form) return;
  // Attributes are read through Element itself: a form's own properties
  // can be shadowed by fields named after them.
  const attribute = (name: string): string | null => Element.prototype.getAttribute.call(form, name);
  const here = root.location?.href ?? "";
  if (!isUploadForm(attribute("action"), attribute("method"), here)) return;
  const input = form.querySelector<HTMLInputElement>('input[type="file"]');
  const zone = form.querySelector<HTMLElement>("[data-upload-drop-zone]");
  const status = form.querySelector<HTMLElement>("[data-upload-status]");
  const link = form.querySelector<HTMLInputElement>('input[name="url"]');
  if (!input || !zone || !status || !link) return;
  const max = Number(input.dataset["max"] ?? "1") || 1;
  let sending = false;

  const busy = (on: boolean): void => {
    sending = on;
    zone.classList.toggle("sending", on);
    for (const button of form.querySelectorAll<HTMLButtonElement>("button[type=submit]")) button.disabled = on;
  };

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
    busy(true);
    form.requestSubmit();
  };

  // A link given with the page (`?url=`) is sent in the background, and
  // the upload replaces this page in the history, so going back skips
  // a form that would only send it again.
  const sendInPlace = async (): Promise<void> => {
    const error = form.querySelector<HTMLElement>("[data-upload-error]");
    busy(true);
    status.textContent = t("upload-sending-link", "Uploading {$url}…", { url: link.value });
    try {
      const body = new FormData(form);
      body.delete("file");
      const response = await fetch(new URL(UPLOADS, here), { method: "POST", body, credentials: "same-origin" });
      if (response.redirected) {
        root.location.replace(response.url);
        return;
      }
      // The form again, saying what went wrong, with the box to tick
      // when a work's many files are to be downloaded.
      const page = new DOMParser().parseFromString(await response.text(), "text/html");
      const message = page.querySelector(".form-error")?.textContent?.trim();
      const confirm = page.querySelector("[data-upload-confirm]");
      if (confirm && !form.querySelector("[data-upload-confirm]")) error?.before(root.importNode(confirm, true));
      if (error) {
        error.textContent = message || t("upload-failed", "The upload failed. Please try again.");
        error.hidden = false;
      }
    } catch {
      if (error) {
        error.textContent = t("upload-failed", "The upload failed. Please try again.");
        error.hidden = false;
      }
    }
    status.textContent = "";
    busy(false);
  };

  input.addEventListener("change", () => {
    if (input.files) send(input.files);
  });
  form.addEventListener("submit", () => busy(true));
  // Coming back to the page (the back button) finds the form usable.
  root.defaultView?.addEventListener("pageshow", () => busy(false));

  // Pasted text that's a link is sent, wherever it's pasted.
  root.addEventListener("paste", (event) => {
    const files = event.clipboardData?.files;
    if (files?.length && typeof DataTransfer !== "undefined") {
      event.preventDefault();
      send(files);
      return;
    }
    const pasted = asLink(event.clipboardData?.getData("text") ?? "");
    if (pasted === null || sending) return;
    event.preventDefault();
    link.value = pasted;
    busy(true);
    form.requestSubmit();
  });

  if (sendsNow(attribute("data-upload-send-now"), here) && link.value) void sendInPlace();

  if (typeof DataTransfer === "undefined") return;
  zone.classList.add("enhanced");
  const hint = form.querySelector<HTMLElement>("[data-upload-hint]");
  if (hint) hint.hidden = false;

  // Files (or a link) dropped anywhere on the page are sent.
  const carries = (event: DragEvent): boolean => {
    const types = event.dataTransfer?.types ?? [];
    return types.includes("Files") || types.includes("text/uri-list");
  };
  let depth = 0;
  root.addEventListener("dragenter", (event) => {
    if (!carries(event)) return;
    event.preventDefault();
    depth++;
    zone.classList.add("dragging");
  });
  root.addEventListener("dragover", (event) => {
    if (!carries(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });
  root.addEventListener("dragleave", () => {
    depth = Math.max(0, depth - 1);
    if (!depth) zone.classList.remove("dragging");
  });
  root.addEventListener("drop", (event) => {
    depth = 0;
    zone.classList.remove("dragging");
    if (!carries(event)) return;
    event.preventDefault();
    const dropped = event.dataTransfer;
    if (dropped?.files.length) {
      send(dropped.files);
      return;
    }
    const url = asLink(dropped?.getData("text/uri-list").split(/\r?\n/).find((line) => line && !line.startsWith("#")) ?? "");
    if (url === null || sending) return;
    link.value = url;
    busy(true);
    form.requestSubmit();
  });
}
