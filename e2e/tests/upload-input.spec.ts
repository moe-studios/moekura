import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// Exercise the real form and committed browser bundle without a database.
const template = readFileSync(new URL("../../crates/web/templates/upload.html", import.meta.url), "utf8");
const form = template.slice(template.indexOf("  <form"), template.indexOf("</form>") + 7)
  .replace(/{%[\s\S]*?%}/g, "").replace(/{{[\s\S]*?}}/g, "");
const script = readFileSync(new URL("../../crates/web/static/js/main.js", import.meta.url), "utf8");

async function transfer(page: Page, kind: "paste" | "drop", names = ["picture.png"]): Promise<boolean> {
  return page.evaluate(({ kind, names }) => {
    const data = new DataTransfer();
    for (const name of names) data.items.add(new File(["image bytes"], name, { type: "image/png" }));
    const event = kind === "paste"
      ? new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true })
      : new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true });
    document.querySelector("#description")!.dispatchEvent(event);
    return event.defaultPrevented;
  }, { kind, names });
}

test.beforeEach(async ({ page }) => {
  await page.route("**/upload", (route) => route.fulfill({ contentType: "text/html", body: form }));
  await page.goto("/upload");
  await page.addScriptTag({ content: script, type: "module" });
  await expect(page.locator("[data-upload-hint]")).toBeVisible();
});

for (const kind of ["paste", "drop"] as const) {
  test(`${kind} selects a file for multipart submission without submitting`, async ({ page }) => {
    await page.locator("#tags").fill("existing_tag");
    expect(await transfer(page, kind)).toBe(true);
    await expect(page.locator("[data-upload-status]")).toHaveText("Selected: picture.png");
    const selected = await page.locator("form").evaluate(async (element) => {
      const data = new FormData(element as HTMLFormElement);
      const file = data.get("file") as File;
      return { name: file.name, content: await file.text(), tags: data.get("tags") };
    });
    expect(selected).toEqual({ name: "picture.png", content: "image bytes", tags: "existing_tag" });
    await expect(page).toHaveURL(/\/upload$/);
  });

  test(`${kind} refuses multiple files and preserves the selection`, async ({ page }) => {
    await page.locator("#file").setInputFiles({ name: "original.png", mimeType: "image/png", buffer: Buffer.from("original") });
    expect(await transfer(page, kind, ["one.png", "two.png"])).toBe(true);
    await expect(page.locator("[data-upload-status]")).toContainText("Choose one file at a time");
    expect(await page.locator("#file").evaluate((el) => (el as HTMLInputElement).files?.[0]?.name)).toBe("original.png");
  });
}

test("text and URL paste/drop retain their native behavior", async ({ page }) => {
  for (const target of ["#url", "#tags", "#source", "#description"]) {
    const prevented = await page.locator(target).evaluate((element) => {
      const data = new DataTransfer();
      data.setData("text/plain", "https://example.com/image.png");
      const paste = new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true });
      const drop = new DragEvent("drop", { dataTransfer: data, bubbles: true, cancelable: true });
      element.dispatchEvent(paste);
      element.dispatchEvent(drop);
      return [paste.defaultPrevented, drop.defaultPrevented];
    });
    expect(prevented).toEqual([false, false]);
  }
  await expect(page.locator("[data-upload-status]")).toBeEmpty();
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
  await transfer(page, "drop");
  await expect(zone).not.toHaveClass(/dragging/);
});

test("the native picker can replace a pasted file", async ({ page }) => {
  await transfer(page, "paste");
  await page.locator("#file").setInputFiles({ name: "picked.webm", mimeType: "video/webm", buffer: Buffer.from("video") });
  await expect(page.locator("[data-upload-status]")).toHaveText("Selected: picked.webm");
  await page.locator("#file").setInputFiles([]);
  await expect(page.locator("[data-upload-status]")).toBeEmpty();
});
