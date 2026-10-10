import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// Exercise the real form and committed browser bundle without a database.
const template = readFileSync(new URL("../../crates/web/templates/upload.html", import.meta.url), "utf8");
const form = template.slice(template.indexOf("  <form"), template.indexOf("</form>") + 7)
  .replace("{{ max_files }}", "3")
  // Files go in pieces of at most 4 bytes.
  .replace("{{ site.upload_piece_bytes }}", "4")
  .replace(/{%[\s\S]*?%}/g, "").replace(/{{\s*t\("([\w-]+)"[\s\S]*?}}/g, "$1").replace(/{{[\s\S]*?}}/g, "");
const script = readFileSync(new URL("../../crates/web/static/js/main.js", import.meta.url), "utf8");

/** The file names in each upload the form sent. */
let sent: string[][] = [];
/** The links the form sent. */
let links: string[] = [];
/** Files sent in pieces, by transfer token: their names, what came, and each piece's size. */
let transfers: Map<string, { name: string; length: number; bytes: string; pieces: number[] }>;
/** Statuses to answer the next pieces with, instead of taking them. */
let failPieces: number[] = [];

async function transfer(page: Page, kind: "paste" | "drop", names = ["picture.png"]): Promise<boolean> {
  return page.evaluate(({ kind, names }) => {
    const data = new DataTransfer();
    for (const name of names) data.items.add(new File(["image bytes"], name, { type: "image/png" }));
    const event = kind === "paste"
      ? new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true })
      : new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true });
    document.querySelector("#url")!.dispatchEvent(event);
    return event.defaultPrevented;
  }, { kind, names });
}

test.beforeEach(async ({ page }) => {
  sent = [];
  links = [];
  transfers = new Map();
  failPieces = [];
  await page.route("**/uploads/new", (route) => route.fulfill({ contentType: "text/html", body: form }));
  // The server's side of the tus protocol, as far as the form uses it.
  await page.route("**/uploads/files", (route) => {
    const headers = route.request().headers();
    const name = Buffer.from((headers["upload-metadata"] ?? "").replace(/^filename /, ""), "base64").toString();
    const token = `t${transfers.size + 1}`;
    transfers.set(token, { name, length: Number(headers["upload-length"]), bytes: "", pieces: [] });
    return route.fulfill({ status: 201, headers: { "Tus-Resumable": "1.0.0", Location: `/uploads/files/${token}` } });
  });
  await page.route("**/uploads/files/*", (route) => {
    const request = route.request();
    const transfer = transfers.get(new URL(request.url()).pathname.split("/").pop()!)!;
    const offset = { "Tus-Resumable": "1.0.0", "Upload-Offset": String(transfer.bytes.length) };
    if (request.method() === "HEAD") return route.fulfill({ status: 200, headers: offset });
    if (request.method() === "DELETE") return route.fulfill({ status: 204 });
    const failure = failPieces.shift();
    if (failure) return route.fulfill({ status: failure, body: failure === 422 ? "Not this file." : "" });
    const piece = request.postDataBuffer()?.toString("latin1") ?? "";
    if (Number(request.headers()["upload-offset"]) !== transfer.bytes.length) return route.fulfill({ status: 409, headers: offset });
    transfer.bytes += piece;
    transfer.pieces.push(piece.length);
    return route.fulfill({ status: 204, headers: { "Tus-Resumable": "1.0.0", "Upload-Offset": String(transfer.bytes.length) } });
  });
  await page.route("**/uploads", (route) => {
    const body = route.request().postDataBuffer()?.toString("latin1") ?? "";
    // Without a file chosen, browsers send an empty one, which the server skips.
    const files = [...body.matchAll(/name="file"; filename="([^"]*)"/g)].map((match) => match[1]!).filter(Boolean);
    const named = [...body.matchAll(/name="transfer"\r\n\r\n([^\r]+)/g)].map((match) => transfers.get(match[1]!));
    // A file named is all there.
    sent.push([...files, ...named.map((t) => (t && t.bytes.length === t.length ? t.name : "incomplete"))]);
    links.push(...[...body.matchAll(/name="url"\r\n\r\n([^\r]+)/g)].map((match) => match[1]!));
    return route.fulfill({ contentType: "text/html", body: "<p>sent</p>" });
  });
  await page.goto("/uploads/new");
  await page.addScriptTag({ content: script, type: "module" });
  await expect(page.locator("[data-upload-hint]")).toBeVisible();
});

for (const kind of ["paste", "drop"] as const) {
  test(`${kind} sends the files right away`, async ({ page }) => {
    expect(await transfer(page, kind, ["one.png", "two.png"])).toBe(true);
    await expect(page.getByText("sent")).toBeVisible();
    expect(sent).toEqual([["one.png", "two.png"]]);
  });

  test(`${kind} refuses more files than the form takes`, async ({ page }) => {
    expect(await transfer(page, kind, ["1.png", "2.png", "3.png", "4.png"])).toBe(true);
    await expect(page.locator("[data-upload-status]")).toContainText("Choose at most 3 files");
    await expect(page).toHaveURL(/\/uploads\/new$/);
    expect(sent).toEqual([]);
  });
}

test("choosing files in the picker sends them", async ({ page }) => {
  await page.locator("#file").setInputFiles({ name: "picked.webm", mimeType: "video/webm", buffer: Buffer.from("video") });
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent).toEqual([["picked.webm"]]);
});

test("files go in pieces, each in its own request, and the form names them", async ({ page }) => {
  expect(await transfer(page, "drop", ["one.png", "two.png"])).toBe(true);
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent).toEqual([["one.png", "two.png"]]);
  // "image bytes", at most 4 bytes at a time.
  expect([...transfers.values()].map((t) => [t.bytes, t.pieces])).toEqual([
    ["image bytes", [4, 4, 3]],
    ["image bytes", [4, 4, 3]],
  ]);
});

test("a piece that fails is sent again from where the server says", async ({ page }) => {
  // A proxy refusing the piece, then a server error.
  failPieces = [413, 502];
  expect(await transfer(page, "drop")).toBe(true);
  await expect(page.getByText("sent", { exact: true })).toBeVisible({ timeout: 10_000 });
  expect(sent).toEqual([["picture.png"]]);
  expect([...transfers.values()][0]!.bytes).toBe("image bytes");
});

test("a refused file says why, and the form stays", async ({ page }) => {
  failPieces = [422];
  expect(await transfer(page, "drop")).toBe(true);
  await expect(page.locator("[data-upload-error]")).toHaveText("Not this file.");
  await expect(page).toHaveURL(/\/uploads\/new$/);
  expect(sent).toEqual([]);
  await expect(page.locator("form button[type=submit]")).toBeEnabled();
});

test("a pasted link is sent right away; other text stays in its field", async ({ page }) => {
  const paste = (text: string) =>
    page.locator("#url").evaluate((element, text) => {
      const data = new DataTransfer();
      data.setData("text/plain", text);
      const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
      element.dispatchEvent(event);
      return event.defaultPrevented;
    }, text);
  expect(await paste("long_hair")).toBe(false);
  const dropped = await page.locator("#url").evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "some words");
    const drop = new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true });
    element.dispatchEvent(drop);
    return drop.defaultPrevented;
  });
  expect(dropped).toBe(false);
  await expect(page.locator("[data-upload-status]")).toBeEmpty();
  expect(sent).toEqual([]);

  // A link (with or without its https://) is sent, wherever it's pasted.
  expect(await paste("example.com/image.png")).toBe(true);
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent).toEqual([[]]);
  expect(links).toEqual(["https://example.com/image.png"]);
});

test("a typed link is sent with the button", async ({ page }) => {
  await page.locator("#url").fill("https://example.com/image.png");
  await page.getByRole("button", { name: "upload-continue" }).click();
  await expect(page.getByText("sent")).toBeVisible();
  expect(links).toEqual(["https://example.com/image.png"]);
});

test("a link the bookmarklet gives with the page is sent at once, and going back skips the form", async ({ page }) => {
  // The server marks the form to be sent only when the bookmarklet's token is right.
  const prefilled = form.replace('id="url" name="url" type="url" value=""', 'id="url" name="url" type="url" value="https://example.com/work"');
  await page.route("**/uploads/new?*", (route) => route.fulfill({ contentType: "text/html", body: prefilled }));
  await page.route("**/uploads", (route) => {
    links.push(...[...(route.request().postData() ?? "").matchAll(/name="url"\r\n\r\n([^\r]*)/g)].map((m) => m[1]!));
    // Requests following a redirect aren't routed: this page is the site's.
    return route.fulfill({ status: 303, headers: { Location: "/wiki" } });
  });
  await page.goto("/uploads/new?token=abc&url=https%3A%2F%2Fexample.com%2Fwork");
  await page.addScriptTag({ content: script, type: "module" });
  await expect(page).toHaveURL(/\/wiki$/);
  expect(links).toEqual(["https://example.com/work"]);
  // Back goes to the page before, not the one sending the link again.
  await page.goBack();
  await expect(page).not.toHaveURL(/url=/);
});

test("a form slipped into another page isn't sent by itself", async ({ page }) => {
  const posted: string[] = [];
  await page.route("**/comments", (route) => {
    posted.push(route.request().url());
    return route.fulfill({ contentType: "text/html", body: "<p>sent</p>" });
  });
  const planted = form.replace('id="url" name="url" type="url" value=""', 'id="url" name="url" type="url" value="https://example.com/work"');
  // Posting elsewhere: not the upload form at all, so pasting a link doesn't send it either.
  await page.route("**/artists/1", (route) => route.fulfill({ contentType: "text/html", body: planted.replace('action="/uploads"', 'action="/comments"') }));
  await page.goto("/artists/1");
  await page.addScriptTag({ content: script, type: "module" });
  const pasted = await page.locator("#url").evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "https://example.com/other");
    const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    element.dispatchEvent(event);
    return event.defaultPrevented;
  });
  expect(pasted).toBe(false);
  await expect(page.locator("[data-upload-hint]")).toBeHidden();
  // Posting to /uploads, but not on a page that shows the upload form: left alone too.
  await page.route("**/artists/2", (route) => route.fulfill({ contentType: "text/html", body: planted }));
  await page.goto("/artists/2");
  await page.addScriptTag({ content: script, type: "module" });
  const pastedThere = await page.locator("#url").evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "https://example.com/other");
    const event = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    element.dispatchEvent(event);
    return event.defaultPrevented;
  });
  expect(pastedThere).toBe(false);
  await expect(page.locator("[data-upload-hint]")).toBeHidden();
  await expect(page.locator("[data-upload-status]")).toBeEmpty();
  // On the upload page, a form slipped in before the real one: neither is trusted.
  await page.route("**/uploads/new?*", (route) => route.fulfill({ contentType: "text/html", body: planted + form }));
  await page.goto("/uploads/new?token=abc&url=https%3A%2F%2Fexample.com%2Fwork");
  await page.addScriptTag({ content: script, type: "module" });
  await expect(page.locator("[data-upload-hint]:visible")).toHaveCount(0);
  await expect(page.locator("[data-upload-status]:not(:empty)")).toHaveCount(0);
  expect(posted).toEqual([]);
  expect(links).toEqual([]);
});

test("file drag highlights the form through child transitions and clears on leave/drop", async ({ page }) => {
  const zone = page.locator("[data-upload-drop-zone]");
  await page.evaluate(() => {
    const data = new DataTransfer();
    data.items.add(new File(["image"], "picture.png", { type: "image/png" }));
    for (const [selector, type] of [["form", "dragenter"], ["#file", "dragenter"], ["form", "dragleave"]]) {
      document.querySelector(selector!)!.dispatchEvent(new DragEvent(type!, { dataTransfer: data, bubbles: true, cancelable: true }));
    }
  });
  await expect(zone).toHaveClass(/dragging/);
  await page.locator("#file").dispatchEvent("dragleave");
  await expect(zone).not.toHaveClass(/dragging/);
});
