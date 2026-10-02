// Following an upload whose files are still downloading: its status is
// checked every quarter of a second, and the page moves on as soon as
// there's something new: to the post form once the first file is ready,
// or to the page again to show what finished. Without scripts, the page
// reloads itself every few seconds instead.

const POLL_MS = 250;
/** Downloads are given up on by the server after ten minutes. */
const GIVE_UP_MS = 11 * 60 * 1000;

export interface FileStatus {
  id: number;
  status: "pending" | "ready" | "failed";
  url: string;
}

export interface Progress {
  pending: number;
  files: FileStatus[];
}

/** Where to go, given `now`: the page of the first ready file when none
 * was ready when the page was shown (`readyBefore`), `"reload"` when
 * something finished (`pendingBefore` were downloading; on a file's own
 * page, `file` is that file), or null to keep waiting. Pure, for tests. */
export function nextStep(
  now: Progress,
  before: { pending: number; ready: number },
  file?: number,
): string | null {
  if (file !== undefined) {
    const mine = now.files.find((f) => f.id === file);
    return mine && mine.status !== "pending" ? "reload" : null;
  }
  const ready = now.files.find((f) => f.status === "ready");
  if (before.ready === 0 && ready) return ready.url;
  return now.pending < before.pending ? "reload" : null;
}

export function enableUploadProgress(root: Document = document): void {
  const follow = root.querySelector<HTMLElement>("[data-upload-follow]");
  const url = follow?.dataset["uploadFollow"];
  if (!follow || !url) return;
  const before = {
    pending: Number(follow.dataset["pending"] ?? "0"),
    ready: Number(follow.dataset["ready"] ?? "0"),
  };
  const fileId = follow.dataset["file"] ? Number(follow.dataset["file"]) : undefined;
  const started = Date.now();
  const check = async (): Promise<void> => {
    try {
      const response = await fetch(url, { credentials: "same-origin", headers: { Accept: "application/json" } });
      if (response.ok) {
        const step = nextStep((await response.json()) as Progress, before, fileId);
        if (step === "reload") {
          root.defaultView?.location.reload();
          return;
        }
        if (step !== null) {
          root.defaultView?.location.replace(step);
          return;
        }
      }
    } catch {
      // Offline for a moment: try again.
    }
    if (Date.now() - started < GIVE_UP_MS) window.setTimeout(() => void check(), POLL_MS);
  };
  window.setTimeout(() => void check(), POLL_MS);
}
