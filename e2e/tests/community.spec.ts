import { expect, test, type Page } from "@playwright/test";
import { admin, logIn, png } from "./helpers.ts";

test.describe.configure({ mode: "serial" });

// Unique per run, so the tests also work against a site used before.
const run = Date.now().toString(36);
const member = { name: `e2e_c_${run}`, password: "correct horse battery" };
const tag = `comic_${run}`;
const pool = `Comic ${run}`;
const posts: string[] = [];
let poolPath = "";

async function upload(page: Page, tags: string): Promise<string> {
  await page.goto("/upload");
  await page.locator("#file").setInputFiles({ name: "page.png", mimeType: "image/png", buffer: png(48, 64) });
  await page.locator("input[name=rating][value=g]").check();
  await page.locator("#tags").fill(tags);
  await page.getByRole("button", { name: "Upload" }).click();
  await expect(page).toHaveURL(/\/posts\/\d+$/);
  return new URL(page.url()).pathname;
}

test("register and upload three pages", async ({ page }) => {
  await page.goto("/register");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password", { exact: true }).fill(member.password);
  await page.getByLabel("Repeat password").fill(member.password);
  await page.getByRole("button", { name: "Register" }).click();
  await expect(page.getByText("Your account is ready")).toBeVisible();
  for (let i = 1; i <= 3; i++) posts.push(await upload(page, `${tag} page_${i}`));
});

test("comment, reply and vote", async ({ page, browser }) => {
  await logIn(page, member.name, member.password);
  await page.goto(posts[0]!);
  await page.getByLabel("Add a comment").fill("What a [b]lovely[/b] first page.");
  await page.getByRole("button", { name: "Post comment" }).click();
  const comment = page.locator("article.comment").first();
  await expect(comment.locator("strong")).toHaveText("lovely");

  // Someone else replies, quoting it, and votes it up.
  const other = await browser.newPage();
  await logIn(other, admin.name, admin.password);
  await other.goto(posts[0]!);
  await other.getByRole("link", { name: "Reply" }).first().click();
  await expect(other.getByLabel("Add a comment")).toHaveValue(new RegExp(`^\\[quote\\]\\n${member.name} said:`));
  await other.getByLabel("Add a comment").press("Control+End");
  await other.getByLabel("Add a comment").pressSequentially("Agreed!");
  await other.getByRole("button", { name: "Post comment" }).click();
  await expect(other.locator("article.comment blockquote")).toContainText("lovely");
  const first = other.locator("article.comment").first();
  await first.getByRole("button", { name: "Vote up" }).click();
  await expect(first.locator(".score")).toHaveText("1");
  await other.close();

  await page.goto(`/posts?tags=${tag}+commentcount:>0`);
  await expect(page.locator("a.card")).toHaveCount(1);
  await page.goto("/comments");
  await expect(page.getByText("Agreed!").first()).toBeVisible();
});

test("make a pool and read it", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto("/pools/new");
  await page.getByLabel("Name").fill(pool);
  await page.getByLabel("Posts, in order").fill(posts.map((p) => p.split("/").pop()).join(" "));
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: pool })).toBeVisible();
  poolPath = new URL(page.url()).pathname;
  await expect(page.locator(".post-grid a.card")).toHaveCount(3);

  // Step through it from the first post with the keyboard.
  await page.locator(".post-grid a.card").first().click();
  await expect(page.locator(".pool-nav.current")).toContainText("1/3");
  await page.keyboard.press("d");
  await expect(page).toHaveURL(new RegExp(`${posts[1]}\\?pool=`));
  await expect(page.locator(".pool-nav.current")).toContainText("2/3");

  // Read it a page at a time; the pool remembers where you were.
  await page.goto(`${poolPath}/read/1`);
  await expect(page.getByText("Page 1 of 3")).toBeVisible();
  await page.keyboard.press("d");
  await expect(page.getByText("Page 2 of 3")).toBeVisible();
  await page.goto(poolPath);
  await expect(page.getByRole("link", { name: "Continue reading from page 2" })).toBeVisible();

  await page.goto(`/posts?tags=ordpool:${pool.replace(" ", "_")}`);
  await expect(page.locator("a.card")).toHaveCount(3);
});

test("save a search and use it", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto(`/posts?tags=${tag}+page_2`);
  await page.getByText("Save this search").click();
  await page.getByLabel("Labels").fill("comics");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.goto("/posts?tags=search:comics");
  await expect(page.locator("a.card")).toHaveCount(1);
  await expect(page.locator(`a.card[href^="${posts[1]}"]`)).toHaveCount(1);
});

test("collect posts in a favorite group", async ({ page }) => {
  await logIn(page, member.name, member.password);
  await page.goto("/favorite_groups/new");
  await page.getByLabel("Name").fill("Best pages");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: "Best pages" })).toBeVisible();
  await page.goto(posts[2]!);
  await page.getByLabel("Group").selectOption({ label: "Best pages" });
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await expect(page.getByRole("button", { name: "Remove from Best pages" })).toBeVisible();
  await page.goto("/posts?tags=favgroup:best_pages");
  await expect(page.locator("a.card")).toHaveCount(1);
});
