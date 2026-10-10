import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// The replace and search-by-image forms, from their templates, with the
// committed browser bundle and a stand-in server.
const script = readFileSync(new URL("../../crates/web/static/js/main.js", import.meta.url), "utf8");

function formFrom(template: string, start: string): string {
  const source = readFileSync(new URL(`../../crates/web/templates/${template}`, import.meta.url), "utf8");
  const from = source.indexOf(start);
  return source.slice(from, source.indexOf("</form>", from) + 7)
    .replaceAll("{{ site.upload_piece_bytes }}", "4")
    .replaceAll("{{ post.id }}", "7")
    // Logged in: the search form sends files in pieces.
    .replace(/{% if me %}([\s\S]*?){% endif %}/g, "$1")
    .replace(/{%[\s\S]*?%}/g, "").replace(/{{\s*t\(["']([\w-]+)["'][\s\S]*?}}/g, "$1").replace(/{{[\s\S]*?}}/g, "");
}

const replace = formFrom("post.html", '<form method="post" action="/posts/{{ post.id }}/replace"');
const search = formFrom("image_search.html", '<form method="post" action="/iqdb_queries"');

/** Files sent in pieces, by transfer token. */
let transfers: Map<string, { name: string; purpose: string; bytes: string }>;
/** What each form sent: its file parts' names and the transfers it named. */
let sent: { files: string[]; transfers: string[] }[];
let failPieces: number[];

async function serve(page: Page, path: string, form: string, action: string): Promise<void> {
  await page.route(`**${path}`, (route) => route.fulfill({ contentType: "text/html", body: `<main>${form}</main>` }));
  await page.route("**/uploads/files", (route) => {
    const headers = route.request().headers();
    const metadata = Object.fromEntries((headers["upload-metadata"] ?? "").split(",").map((pair) => {
      const [key, value] = pair.split(" ");
      return [key, Buffer.from(value ?? "", "base64").toString()];
    }));
    const token = `t${transfers.size + 1}`;
    transfers.set(token, { name: metadata["filename"]!, purpose: metadata["purpose"] ?? "upload", bytes: "" });
    return route.fulfill({ status: 201, headers: { "Tus-Resumable": "1.0.0", Location: `/uploads/files/${token}` } });
  });
  await page.route("**/uploads/files/*", (route) => {
    const request = route.request();
    const transfer = transfers.get(new URL(request.url()).pathname.split("/").pop()!)!;
    if (request.method() === "DELETE") return route.fulfill({ status: 204 });
    const failure = failPieces.shift();
    if (failure) return route.fulfill({ status: failure, body: "Not this file." });
    transfer.bytes += request.postDataBuffer()?.toString("latin1") ?? "";
    return route.fulfill({ status: 204, headers: { "Tus-Resumable": "1.0.0", "Upload-Offset": String(transfer.bytes.length) } });
  });
  await page.route(`**${action}`, (route) => {
    // The search page is at its form's address too.
    if (route.request().method() === "GET") return route.fallback();
    const body = route.request().postDataBuffer()?.toString("latin1") ?? "";
    sent.push({
      files: [...body.matchAll(/name="file"; filename="([^"]*)"/g)].map((m) => m[1]!).filter(Boolean),
      transfers: [...body.matchAll(/name="transfer"\r\n\r\n([^\r]+)/g)].map((m) => m[1]!),
    });
    return route.fulfill({ contentType: "text/html", body: "<p>sent</p>" });
  });
  await page.goto(path);
  await page.addScriptTag({ content: script, type: "module" });
}

test.beforeEach(() => {
  transfers = new Map();
  sent = [];
  failPieces = [];
});

test("a new file for a post goes in pieces, asked about once", async ({ page }) => {
  await serve(page, "/posts/7", replace, "/posts/7/replace");
  let asked = 0;
  page.on("dialog", (dialog) => {
    asked++;
    void dialog.accept();
  });
  await page.locator("#replace-file").setInputFiles({ name: "better.png", mimeType: "image/png", buffer: Buffer.from("better image") });
  await page.locator("#replace-reason").fill("Larger");
  await page.locator("form button[type=submit]").click();
  await expect(page.getByText("sent")).toBeVisible();
  expect(asked).toBe(1);
  expect(sent).toEqual([{ files: [], transfers: ["t1"] }]);
  expect(transfers.get("t1")).toEqual({ name: "better.png", purpose: "replace", bytes: "better image" });
});

test("declining the question sends nothing", async ({ page }) => {
  await serve(page, "/posts/7", replace, "/posts/7/replace");
  page.on("dialog", (dialog) => void dialog.dismiss());
  await page.locator("#replace-file").setInputFiles({ name: "better.png", mimeType: "image/png", buffer: Buffer.from("better image") });
  await page.locator("form button[type=submit]").click();
  await page.waitForTimeout(300);
  expect(transfers.size).toBe(0);
  expect(sent).toEqual([]);
});

test("a picture to search with goes in pieces; a link goes as it is", async ({ page }) => {
  await serve(page, "/iqdb_queries", search, "/iqdb_queries");
  await page.locator("#iqdb-file").setInputFiles({ name: "like.png", mimeType: "image/png", buffer: Buffer.from("a picture") });
  await page.locator("form button[type=submit]").click();
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent).toEqual([{ files: [], transfers: ["t1"] }]);
  expect(transfers.get("t1")).toEqual({ name: "like.png", purpose: "search", bytes: "a picture" });

  await page.goto("/iqdb_queries");
  await page.addScriptTag({ content: script, type: "module" });
  await page.locator("#iqdb-url").fill("https://example.com/a.png");
  await page.locator("form button[type=submit]").click();
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent[1]).toEqual({ files: [], transfers: [] });
  expect(transfers.size).toBe(1);
});

test("a refused file says why, and the form stays", async ({ page }) => {
  await serve(page, "/iqdb_queries", search, "/iqdb_queries");
  failPieces = [422];
  await page.locator("#iqdb-file").setInputFiles({ name: "like.png", mimeType: "image/png", buffer: Buffer.from("a picture") });
  await page.locator("form button[type=submit]").click();
  await expect(page.locator("[data-transfer-error]")).toHaveText("Not this file.");
  await expect(page.locator("form button[type=submit]")).toBeEnabled();
  expect(sent).toEqual([]);
});
