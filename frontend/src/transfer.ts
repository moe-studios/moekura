// Sends a file in pieces with the tus protocol (1.0.0), each piece in its
// own request, so a proxy or CDN that limits how large a request can be
// (Cloudflare's limit is 100 MB) doesn't limit the file. Pieces are sized
// to take about ten seconds on the connection, well within the server's
// request timeout, and at most the size the server says. A piece that
// fails is sent again from where the server says the file got to, smaller.

/** Where transfers are begun. */
export const TRANSFERS = "/uploads/files";
const TUS_VERSION = "1.0.0";
const MB = 1024 * 1024;
/** The first piece's size, before the connection's speed is known. */
const FIRST_PIECE = 4 * MB;
/** The smallest piece, sent when a connection is slow or keeps failing. */
const MIN_PIECE = 256 * 1024;
/** How long a piece should take to send. */
const PIECE_SECONDS = 10;
/** Tries in a row that may fail before the file is given up on. */
export const MAX_TRIES = 5;
/** Statuses after which the piece is sent again: no answer, a timeout, a
 * piece too large for a proxy, a conflict over where the file got to, and
 * the server or a proxy failing (Cloudflare's 52x included). */
const RETRY = new Set([0, 408, 409, 413, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524]);

/** An answer to a request. */
export interface Answer {
  status: number;
  header(name: string): string | null;
  text: string;
}

/** How requests are made: through the browser, or a fake in tests. */
export interface Transport {
  /** A request without a body (or whose sending isn't followed). */
  request(method: string, url: string, headers: Record<string, string>): Promise<Answer>;
  /** Sends `body`, saying how much of it went as it goes. */
  piece(url: string, body: Blob, headers: Record<string, string>, sent: (bytes: number) => void): Promise<Answer>;
}

/** Why a file couldn't be sent: the server's words, when it gave some. */
export class TransferError extends Error {}

/**
 * The size of the next piece: about {@link PIECE_SECONDS} at the speed the
 * last one went (`bytesPerSecond`, `null` before one has), at most four
 * times the last (`previous`) and at most `max`.
 */
export function pieceSize(previous: number | null, bytesPerSecond: number | null, max: number): number {
  const floor = Math.min(MIN_PIECE, max);
  if (previous === null || bytesPerSecond === null) return Math.max(floor, Math.min(FIRST_PIECE, max));
  const wanted = Math.min(bytesPerSecond * PIECE_SECONDS, previous * 4, max);
  return Math.max(floor, Math.floor(wanted));
}

/** A smaller piece, after one of `size` failed: half, down to {@link MIN_PIECE}. */
export function smaller(size: number): number {
  return size <= MIN_PIECE ? size : Math.max(MIN_PIECE, Math.floor(size / 2));
}

/** The `Upload-Metadata` naming a file `name`: `filename` and the name in base64. */
export function metadata(name: string): string {
  let binary = "";
  for (const byte of new TextEncoder().encode(name)) binary += String.fromCharCode(byte);
  return `filename ${btoa(binary)}`;
}

/** The token at the end of a transfer's URL. */
export function tokenOf(url: string): string {
  return new URL(url, "http://x").pathname.split("/").pop() ?? "";
}

export interface SendOptions {
  /** The largest piece the server takes, in bytes. */
  maxPiece: number;
  /** The page's URL, which the transfers' are relative to. */
  base: string;
  transport?: Transport;
  /** Bytes of the file the server has, as they go. */
  progress?: (sent: number) => void;
  /** Called before a failed piece is sent again. */
  retrying?: () => void;
  /** Waits `ms` milliseconds; in tests, not at all. */
  wait?: (ms: number) => Promise<void>;
  /** Clock in milliseconds, for measuring speed. */
  now?: () => number;
}

const pause = (ms: number): Promise<void> => new Promise((done) => setTimeout(done, ms));

/**
 * Sends `file` named `name`, returning its transfer's token, which a form
 * then names in a `transfer` field. If it can't be sent, what was sent is
 * removed, and a {@link TransferError} says why.
 */
export async function sendFile(file: Blob, name: string, options: SendOptions): Promise<string> {
  const transport = options.transport ?? browser;
  const wait = options.wait ?? pause;
  const now = options.now ?? (() => performance.now());
  const tus = { "Tus-Resumable": TUS_VERSION };
  const created = await transport.request("POST", new URL(TRANSFERS, options.base).href, {
    ...tus,
    "Upload-Length": String(file.size),
    "Upload-Metadata": metadata(name),
  });
  const location = created.header("Location");
  if (created.status !== 201 || !location) throw refusal(created);
  const url = new URL(location, options.base).href;
  try {
    let offset = 0;
    let size: number | null = null;
    let speed: number | null = null;
    let tries = 0;
    while (offset < file.size) {
      size = tries ? size! : pieceSize(size, speed, options.maxPiece);
      const end = Math.min(file.size, offset + size);
      const started = now();
      const from = offset;
      const answer = await transport.piece(
        url,
        file.slice(from, end),
        { ...tus, "Upload-Offset": String(from), "Content-Type": "application/offset+octet-stream" },
        (sent) => options.progress?.(from + sent),
      );
      const got = Number(answer.header("Upload-Offset"));
      if (answer.status === 204 && got === end) {
        speed = (end - from) / Math.max(0.001, (now() - started) / 1000);
        offset = end;
        tries = 0;
        options.progress?.(offset);
        continue;
      }
      if (!RETRY.has(answer.status) || ++tries >= MAX_TRIES) throw refusal(answer);
      options.retrying?.();
      size = smaller(size);
      speed = null;
      await wait(1000 * 2 ** (tries - 1));
      // The server says where the file got to: a piece may have arrived
      // though its answer didn't.
      const where = await transport.request("HEAD", url, tus).catch(() => null);
      if (where?.status === 200) offset = Number(where.header("Upload-Offset")) || 0;
      else if (where && !RETRY.has(where.status)) throw refusal(where);
      options.progress?.(offset);
    }
    return tokenOf(url);
  } catch (error) {
    void transport.request("DELETE", url, tus).catch(() => undefined);
    throw error instanceof TransferError ? error : new TransferError("");
  }
}

/** A {@link TransferError} with the server's words, if a short text. */
function refusal(answer: Answer): TransferError {
  const text = answer.text.trim();
  const plain = text && text.length < 500 && !text.startsWith("<");
  return new TransferError(plain ? text : "");
}

/** Requests through the browser: `fetch`, and XMLHttpRequest for pieces, whose sending it can follow. */
const browser: Transport = {
  async request(method, url, headers) {
    const response = await fetch(url, { method, headers, credentials: "same-origin" });
    return { status: response.status, header: (name) => response.headers.get(name), text: method === "HEAD" ? "" : await response.text() };
  },
  piece(url, body, headers, sent) {
    return new Promise((done) => {
      const xhr = new XMLHttpRequest();
      xhr.open("PATCH", url);
      xhr.withCredentials = true;
      for (const [name, value] of Object.entries(headers)) xhr.setRequestHeader(name, value);
      xhr.upload.addEventListener("progress", (event) => sent(event.loaded));
      const answer = (): void => done({ status: xhr.status, header: (name) => xhr.getResponseHeader(name), text: xhr.responseText ?? "" });
      xhr.addEventListener("load", answer);
      xhr.addEventListener("error", answer);
      xhr.addEventListener("timeout", answer);
      xhr.addEventListener("abort", answer);
      xhr.send(body);
    });
  },
};
