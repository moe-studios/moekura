import { expect, test } from "@playwright/test";
import { png } from "./helpers.ts";

test.describe.configure({ mode: "serial" });

const run = Date.now().toString(36);
const member = { name: `e2e_t_${run}`, password: "correct horse battery" };
const tag = `script_${run}`;

test("tag posts with a tag script", async ({ page }) => {
  await page.goto("/register");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password", { exact: true }).fill(member.password);
  await page.getByLabel("Repeat password").fill(member.password);
  await page.getByRole("button", { name: "Register" }).click();
  await expect(page.getByText("Your account is ready")).toBeVisible();
  for (let i = 0; i < 2; i++) {
    await page.goto("/upload");
    await page.locator("#file").setInputFiles({ name: "p.png", mimeType: "image/png", buffer: png(24, 24) });
    await page.locator("input[name=rating][value=g]").check();
    await page.locator("#tags").fill(tag);
    await page.getByRole("button", { name: "Upload" }).click();
    await expect(page).toHaveURL(/\/posts\/\d+$/);
  }

  await page.goto(`/posts?tags=${tag}`);
  await page.getByLabel("Script", { exact: true }).fill(`scripted_${run} rating:s`);
  await page.getByLabel("Apply by clicking posts").check();
  const cards = page.locator(".post-grid a.card");
  await cards.nth(0).click();
  await expect(cards.nth(0)).toHaveClass(/script-ok/);
  await cards.nth(1).click();
  await expect(cards.nth(1)).toHaveClass(/script-ok/);
  await expect(page).toHaveURL(new RegExp(`/posts\\?tags=${tag}`));

  await page.goto(`/posts?tags=scripted_${run}+rating:s`);
  await expect(page.locator(".post-grid a.card")).toHaveCount(2);
});

test("request a bulk update and discuss it", async ({ page }) => {
  await page.goto("/login");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password").fill(member.password);
  await page.getByRole("button", { name: "Log in" }).click();
  await page.goto("/tags/requests/new");
  await page.getByLabel("Title").fill(`Tidy ${run}`);
  await page.getByLabel("Script").fill(`imply ${tag} -> tagged_${run}\nalias oops`);
  await page.getByRole("button", { name: "Send request" }).click();
  await expect(page.getByRole("alert")).toContainText("line 2");
  await page.getByLabel("Script").fill(`imply ${tag} -> tagged_${run}`);
  await page.getByRole("button", { name: "Send request" }).click();
  await expect(page.locator(".bulk-script")).toContainText(`imply ${tag} -> tagged_${run}`);
  await page.getByLabel("Add a comment").fill("Every scripted post should say so.");
  await page.getByRole("button", { name: "Post comment" }).click();
  await expect(page.getByText("Every scripted post should say so.")).toBeVisible();
});
