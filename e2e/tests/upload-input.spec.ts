import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// Exercise the real form and committed browser bundle without a database.
const template = readFileSync(new URL("../../crates/web/templates/upload.html", import.meta.url), "utf8");
const form = template.slice(template.indexOf("  <form"), template.indexOf("</form>") + 7)
  .replace("{{ max_files }}", "3")
  .replace(/{%[\s\S]*?%}/g, "").replace(/{{\s*t\("([\w-]+)"[\s\S]*?}}/g, "$1").replace(/{{[\s\S]*?}}/g, "");
const script = readFileSync(new URL("../../crates/web/static/js/main.js", import.meta.url), "utf8");

/** The file names in each upload the form sent. */
let sent: string[][] = [];

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
  await page.route("**/uploads/new", (route) => route.fulfill({ contentType: "text/html", body: form }));
  await page.route("**/uploads", (route) => {
    const body = route.request().postDataBuffer()?.toString("latin1") ?? "";
    // Without a file chosen, browsers send an empty one, which the server skips.
    sent.push([...body.matchAll(/name="file"; filename="([^"]*)"/g)].map((match) => match[1]!).filter(Boolean));
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

test("a link is sent with the button, and text pastes stay in their field", async ({ page }) => {
  const prevented = await page.locator("#url").evaluate((element) => {
    const data = new DataTransfer();
    data.setData("text/plain", "https://example.com/image.png");
    const paste = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
    const drop = new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true });
    element.dispatchEvent(paste);
    element.dispatchEvent(drop);
    return [paste.defaultPrevented, drop.defaultPrevented];
  });
  expect(prevented).toEqual([false, false]);
  await expect(page.locator("[data-upload-status]")).toBeEmpty();
  await page.locator("#url").fill("https://example.com/image.png");
  await page.getByRole("button", { name: "upload-continue" }).click();
  await expect(page.getByText("sent")).toBeVisible();
  expect(sent).toEqual([[]]);
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
