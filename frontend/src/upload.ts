// The upload form sends files as soon as they're chosen, as Danbooru's
// does: from the picker, the clipboard or a drop anywhere on the page.
// Each file is sent in pieces first (see transfer.ts), so no request has
// to carry a whole file, let alone all of them; the form then names the
// files sent. Without scripts, the form sends the files itself. A link is
// sent as soon as it's pasted anywhere on the page, or when the user's
// bookmarklet opened the page with one; a typed one is sent with the
// button.

import { t } from "./i18n.ts";
import { TRANSFERS, TransferError, sendFile } from "./transfer.ts";

/** Where the upload form sends what it's given. */
const UPLOADS = "/uploads";
/** The upload page, the only one that sends a link given with it. */
const UPLOAD_PAGE = "/uploads/new";
/** The pages that show the upload form: the upload page, and the form again after a refusal. */
const FORM_PAGES = [UPLOAD_PAGE, UPLOADS, "/upload"];

/**
 * Whether a form with these `action` and `method` attributes, on the page
 * at `page`, is the upload form: one posting to this site's `/uploads`,
 * on a page that shows it. Only that form is sent by itself, so markup
 * slipped into some other page can't have a form of its own sent as the
 * viewer.
 */
export function isUploadForm(action: string | null, method: string | null, page: string): boolean {
  if (action === null || method?.toLowerCase() !== "post") return false;
  try {
    const here = new URL(page);
    const target = new URL(action, here);
    return FORM_PAGES.includes(here.pathname) && target.origin === here.origin && target.pathname === UPLOADS && target.search === "";
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

  const piece = Number(attribute("data-upload-piece")) || 50 * 1024 * 1024;

  const send = (files: FileList): void => {
    if (sending || files.length === 0) return;
    if (files.length > max) {
      status.textContent = t("upload-too-many", "Choose at most {$max} files at once.", { max });
      return;
    }
    void sendInPieces(Array.from(files));
  };

  // Sends each file in pieces, then the form naming them.
  const sendInPieces = async (files: File[]): Promise<void> => {
    const error = form.querySelector<HTMLElement>("[data-upload-error]");
    if (error) error.hidden = true;
    busy(true);
    const total = files.reduce((sum, file) => sum + file.size, 0) || 1;
    let before = 0;
    const show = (sent: number): void => {
      const percent = Math.min(100, Math.floor(((before + sent) * 100) / total));
      status.textContent = files.length === 1
        ? t("upload-progress-one", "Uploading {$name}… {$percent}%", { name: files[0]!.name, percent })
        : t("upload-progress-many", "Uploading {$count} files… {$percent}%", { count: files.length, percent });
    };
    const tokens: string[] = [];
    try {
      for (const file of files) {
        show(0);
        const retrying = (): void => {
          status.textContent = t("upload-retrying", "The connection faltered; trying again…");
        };
        tokens.push(await sendFile(file, file.name, { maxPiece: piece, base: here, progress: show, retrying }));
        before += file.size;
      }
    } catch (failure) {
      // The files sent already are given up on too.
      for (const token of tokens) {
        const url = new URL(`${TRANSFERS}/${token}`, here);
        void fetch(url, { method: "DELETE", headers: { "Tus-Resumable": "1.0.0" }, credentials: "same-origin" }).catch(() => undefined);
      }
      if (error) {
        error.textContent = (failure instanceof TransferError && failure.message) || t("upload-failed", "The upload failed. Please try again.");
        error.hidden = false;
      }
      status.textContent = "";
      busy(false);
      return;
    }
    // The form names the files instead of sending them again.
    input.value = "";
    forgetTransfers();
    for (const token of tokens) {
      const field = root.createElement("input");
      field.type = "hidden";
      field.name = "transfer";
      field.value = token;
      field.setAttribute("data-upload-transfer", "");
      form.append(field);
    }
    form.requestSubmit();
  };

  // Files named by an earlier send are used up.
  const forgetTransfers = (): void => {
    for (const field of form.querySelectorAll("input[data-upload-transfer]")) field.remove();
  };

  // A link the bookmarklet gave with the page (`?url=`) is sent in the
  // background, and the upload replaces this page in the history, so
  // going back skips a form that would only send it again.
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
  root.defaultView?.addEventListener("pageshow", () => {
    forgetTransfers();
    busy(false);
  });

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
