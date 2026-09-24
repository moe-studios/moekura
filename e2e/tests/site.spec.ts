import { expect, test, type Page } from "@playwright/test";
import { admin, logIn, png } from "./helpers.ts";

test.describe.configure({ mode: "serial" });

// Unique per run, so the tests also work against a site used before.
const run = Date.now().toString(36);
const member = { name: `e2e_${run}`, password: "correct horse battery" };
const tag = `tag_${run}`;
let postPath = "";

const tagLink = (page: Page, name: string) => page.locator("a.tag", { hasText: new RegExp(`^${name}$`) });

test("register an account", async ({ page }) => {
  await page.goto("/register");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password", { exact: true }).fill(member.password);
  await page.getByLabel("Repeat password").fill(member.password);
  await page.getByRole("button", { name: "Register" }).click();
  await expect(page.getByText("Your account is ready")).toBeVisible();
});

test("upload a picture with tags", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto("/upload");
  await page.locator("#file").setInputFiles({ name: "e2e.png", mimeType: "image/png", buffer: png() });
  await page.locator("input[name=rating][value=g]").check();
  await page.locator("#tags").fill(`${tag} solid_colour`);
  await page.getByRole("button", { name: "Upload" }).click();
  await expect(page).toHaveURL(/\/posts\/\d+$/);
  postPath = new URL(page.url()).pathname;
  await expect(tagLink(page, tag)).toBeVisible();
});

test("edit the tags", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto(postPath);
  await page.locator("details", { has: page.locator("#edit-tags") }).locator("summary").click();
  await page.locator("#edit-tags").fill(`${tag} solid_colour square`);
  await page.getByRole("button", { name: "Save" }).click();
  await expect(tagLink(page, "square")).toBeVisible();
  await page.goto(`${postPath}/history`);
  await expect(page.getByText("Version 2")).toBeVisible();
});

test("search finds it", async ({ page }) => {
  await page.goto(`/posts?tags=${tag}+square`);
  await expect(page.locator(`a.card[href^="${postPath}"]`)).toHaveCount(1);
  await page.goto(`/posts?tags=${tag}+-square`);
  await expect(page.getByText("Nothing found")).toBeVisible();
});

test("describe the tag in the wiki", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto(`/wiki/${tag}`);
  await page.getByRole("link", { name: "Start this page" }).click();
  await page.getByLabel("Text").fill(`Posts made by the [b]end-to-end[/b] tests. See [[square]].`);
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.locator(".markup strong")).toHaveText("end-to-end");
  await page.goto(`/posts?tags=${tag}`);
  await expect(page.locator(".wiki-excerpt")).toContainText("Posts made by the end-to-end tests.");
});

test("favorite it", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto(postPath);
  const favorite = page.getByRole("button", { name: "Favorite" });
  await expect(favorite).toHaveAttribute("aria-pressed", "false");
  await favorite.click();
  await expect(favorite).toHaveAttribute("aria-pressed", "true");
  await page.reload();
  await expect(page.getByRole("button", { name: "Favorite" })).toHaveAttribute("aria-pressed", "true");
});

test("staff delete it", async ({ page, browser }) => {
  await logIn(page, admin.name, admin.password);
  await page.goto(postPath);
  await page.locator("details", { has: page.locator("#delete-reason") }).locator("summary").click();
  await page.locator("#delete-reason").fill("end-to-end test");
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByText("end-to-end test")).toBeVisible();

  // Visitors don't see deleted posts.
  const visitor = await browser.newPage();
  const response = await visitor.goto(postPath);
  expect(response?.status()).toBe(404);
  await visitor.close();
});

test("a private site keeps visitors out", async ({ page, browser }) => {
  await logIn(page, admin.name, admin.password);
  const setVisitorsCanView = async (allowed: boolean) => {
    await page.goto("/admin/roles");
    const anonymous = page.locator("form").filter({ has: page.locator('input[name=name][value="Anonymous"]') });
    await anonymous.locator("input[name=view_posts]").setChecked(allowed);
    await anonymous.getByRole("button", { name: "Save Anonymous" }).click();
    await expect(page.getByText("Saved.")).toBeVisible();
  };

  await setVisitorsCanView(false);
  try {
    const visitor = await browser.newPage();
    await visitor.goto("/");
    await expect(visitor).toHaveURL(/\/login\?next=/);
    const api = await visitor.request.get("/api/v1/posts");
    expect(api.status()).toBe(401);
    await visitor.close();
  } finally {
    await setVisitorsCanView(true);
  }
});
