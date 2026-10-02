import { expect, test } from "@playwright/test";
import { png, uploadPost } from "./helpers.ts";

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
    await uploadPost(page, { name: "p.png", buffer: png(24, 24) }, tag);
  }

  await page.goto(`/posts?tags=${tag}`);
  // One search, above the posts, and the script only shows in its mode.
  await expect(page.getByRole("search")).toHaveCount(1);
  const searchBox = (await page.getByRole("search").boundingBox())!;
  expect(searchBox.y + searchBox.height).toBeLessThan((await page.locator(".post-grid").boundingBox())!.y);
  await page.keyboard.press("/");
  await expect(page.getByRole("search").locator("input[name=tags]")).toBeFocused();
  const script = page.getByLabel("Tag script", { exact: true }).and(page.locator("input"));
  await expect(script).toBeHidden();
  await page.getByLabel("On click").selectOption("Tag script");
  await script.fill(`scripted_${run} rating:s`);
  const cards = page.locator(".post-grid a.post-card");
  await cards.nth(0).click();
  await expect(cards.nth(0)).toHaveClass(/script-ok/);
  await cards.nth(1).click();
  await expect(cards.nth(1)).toHaveClass(/script-ok/);
  await expect(page).toHaveURL(new RegExp(`/posts\\?tags=${tag}`));
  // The mode lasts to the next page, until it's set back.
  await page.reload();
  await expect(script).toHaveValue(`scripted_${run} rating:s`);
  await page.getByLabel("On click").selectOption("View the post");
  await expect(script).toBeHidden();

  await page.goto(`/posts?tags=scripted_${run}+rating:s`);
  await expect(page.locator(".post-grid a.post-card")).toHaveCount(2);
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
