import { expect, test } from "@playwright/test";
import { logIn, png } from "./helpers.ts";

test.describe.configure({ mode: "serial" });

// Its own account: the admin's logins are rate limited, and other tests
// and the Danbooru client check use them.
const member = { name: `e2e_n_${Date.now().toString(36)}`, password: "correct horse battery" };
let postPath = "";

test("draw a note, move it and change it", async ({ page }) => {
  await page.goto("/register");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password", { exact: true }).fill(member.password);
  await page.getByLabel("Repeat password").fill(member.password);
  await page.getByRole("button", { name: "Register" }).click();
  await expect(page.getByText("Your account is ready")).toBeVisible();
  await page.goto("/upload");
  await page.locator("#file").setInputFiles({ name: "page.png", mimeType: "image/png", buffer: png(400, 300) });
  await page.locator("input[name=rating][value=g]").check();
  await page.locator("#tags").fill("notes_test");
  await page.getByRole("button", { name: "Upload" }).click();
  await expect(page).toHaveURL(/\/posts\/\d+$/);
  postPath = new URL(page.url()).pathname;

  // Draw a box over the image.
  await page.getByRole("button", { name: "Edit notes" }).click();
  const svg = page.locator("svg.notes");
  const area = (await svg.boundingBox())!;
  await page.mouse.move(area.x + area.width * 0.1, area.y + area.height * 0.1);
  await page.mouse.down();
  await page.mouse.move(area.x + area.width * 0.4, area.y + area.height * 0.3, { steps: 5 });
  await page.mouse.up();
  await page.getByLabel("Note", { exact: true }).fill("Good [b]morning[/b]");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.locator("rect.note-box")).toHaveCount(1);
  const box = page.locator("rect.note-box");
  const x = Number(await box.getAttribute("x"));
  expect(x).toBeGreaterThanOrEqual(35);
  expect(x).toBeLessThanOrEqual(45);

  // Pointing at it shows the text.
  await box.hover();
  await expect(page.locator(".note-popup strong")).toHaveText("morning");

  // Move it right.
  await page.getByRole("button", { name: "Edit notes" }).click();
  const at = (await box.boundingBox())!;
  await page.mouse.move(at.x + at.width / 3, at.y + at.height / 3);
  await page.mouse.down();
  await page.mouse.move(at.x + at.width / 3 + area.width * 0.2, at.y + at.height / 3, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => Number(await page.locator("rect.note-box").getAttribute("x"))).toBeGreaterThan(x + 50);

  // Change its text.
  await page.getByRole("button", { name: "Edit notes" }).click();
  await page.locator("rect.note-box").click();
  await page.getByLabel("Note", { exact: true }).fill("Good evening");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.locator(".note-list")).toContainText("Good evening");

  await page.goto(`/posts?tags=note:evening+notes_test`);
  await expect(page.locator(`a.card[href^="${postPath}"]`)).toHaveCount(1);
});

test("hide notes, and revert from the history", async ({ page }) => {
  await page.goto(postPath);
  await page.getByRole("button", { name: "Hide notes" }).click();
  await expect(page.locator("svg.notes")).toBeHidden();
  await page.reload();
  await expect(page.locator("svg.notes")).toBeHidden();
  await page.getByRole("button", { name: "Show notes" }).click();
  await expect(page.locator("svg.notes")).toBeVisible();

  await logIn(page, member.name, member.password);
  await page.goto(`${postPath}/notes/history`);
  await page.getByRole("button", { name: "Revert to this" }).last().click();
  await page.goto(postPath);
  await expect(page.locator(".note-list strong")).toHaveText("morning");
});
