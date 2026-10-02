import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { admin, gif, logIn, png, stageUpload } from "./helpers.ts";

let adminCookies: Awaited<ReturnType<BrowserContext["cookies"]>> | undefined;

async function authenticateAdmin(page: Page): Promise<void> {
  if (!adminCookies) {
    await logIn(page, admin.name, admin.password);
    adminCookies = await page.context().cookies();
  } else {
    await page.context().addCookies(adminCookies);
  }
}

async function expectPageFits(page: Page): Promise<void> {
  await expect.poll(() => page.evaluate(() =>
    document.documentElement.scrollWidth - document.documentElement.clientWidth,
  )).toBeLessThanOrEqual(1);
}

test("public pages fit phone, tablet and desktop viewports", async ({ page }) => {
  for (const width of [320, 375, 768, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    for (const path of ["/", "/login", "/register", "/tags", "/pools", "/comments", "/post_versions", "/tags/related", "/api/docs"]) {
      await page.goto(path);
      await expectPageFits(page);
    }
    const search = page.getByRole("search");
    const header = page.locator(".site-header");
    if (width < 700) {
      const searchBox = await search.boundingBox();
      const headerBox = await header.boundingBox();
      expect(searchBox!.width).toBeGreaterThan(headerBox!.width - 40);
      // Site names and footer descriptions can contain long, unbroken text.
      await page.locator(".brand").evaluate(e => { e.textContent = "x".repeat(64); });
      await page.locator(".site-footer").evaluate(e => {
        const description = document.createElement("p");
        description.className = "description";
        description.textContent = "x".repeat(300);
        e.append(description);
      });
      await expectPageFits(page);
    }
  }
});

test("admin settings and permissions fit narrow screens", async ({ page }) => {
  await authenticateAdmin(page);
  for (const width of [320, 375, 768]) {
    await page.setViewportSize({ width, height: 800 });
    for (const path of ["/admin/roles", "/admin/settings", "/admin/users", "/settings", "/moderation/queue"]) {
      await page.goto(path);
      await expectPageFits(page);
    }
  }
});

test("wrapped history filters keep labels above their own fields", async ({ page }) => {
  for (const width of [320, 768]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/post_versions");
    for (const label of await page.locator(".filters label[for]").all()) {
      const id = await label.getAttribute("for");
      const labelBox = await label.boundingBox();
      const inputBox = await page.locator(`#${id}`).boundingBox();
      expect(labelBox!.y + labelBox!.height).toBeLessThanOrEqual(inputBox!.y);
      expect(Math.abs(labelBox!.x - inputBox!.x)).toBeLessThan(1);
    }
    await expectPageFits(page);
  }
});

test("expanded upload fields fill the form and hidden helpers stay hidden", async ({ page }) => {
  await authenticateAdmin(page);
  await page.setViewportSize({ width: 320, height: 800 });
  await page.goto(await stageUpload(page, png(64, 48)));
  await expect(page.locator("[data-artist-finder]")).toBeHidden();
  await page.getByText("Artist's commentary", { exact: false }).click();
  const title = page.locator("#commentary_title");
  const description = page.locator("#commentary_description");
  await expect(title).toBeVisible();
  await expect(description).toBeVisible();
  const titleBox = await title.boundingBox();
  const descriptionBox = await description.boundingBox();
  expect(titleBox!.y + titleBox!.height).toBeLessThan(descriptionBox!.y);
  expect(Math.abs(titleBox!.width - descriptionBox!.width)).toBeLessThan(1);
  await expectPageFits(page);
});

test("populated tables scroll locally and large thumbnails fit the results column", async ({ page }) => {
  await authenticateAdmin(page);
  const tag = `layout_${Date.now().toString(36)}_long_tag_name_for_sidebar_and_table_wrapping`;
  const upload = await page.request.post("/upload", {
    multipart: {
      file: { name: "layout.png", mimeType: "image/png", buffer: png(960, 640) },
      rating: "g", tags: tag,
    },
  });
  expect(upload.ok()).toBeTruthy();
  await page.setViewportSize({ width: 320, height: 800 });
  await page.goto(`/tags?name=${tag}`);
  const table = page.getByRole("region", { name: "Tags table" });
  await expect(table).toContainText(tag);
  await expectPageFits(page);
  expect(await table.evaluate(e => e.scrollWidth > e.clientWidth)).toBe(true);
  await table.focus();
  await page.keyboard.press("ArrowRight");
  await expect.poll(() => table.evaluate(e => e.scrollLeft)).toBeGreaterThan(0);

  for (const width of [320, 375, 900, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto(`/posts?tags=${tag}`);
    await expect(page.locator(".post-grid .post-card")).toHaveCount(1);
    // Exercise the same layout selected by the large-thumbnail preference.
    await page.evaluate(() => { document.documentElement.dataset.thumbs = "large"; });
    await expectPageFits(page);
    const cardBox = await page.locator(".post-grid .post-card").boundingBox();
    const resultsBox = await page.locator(".results").boundingBox();
    expect(cardBox!.width).toBeLessThanOrEqual(resultsBox!.width + 1);
  }
});

test("thumbnails on the comments page carry their own badges", async ({ page }) => {
  await authenticateAdmin(page);
  const upload = await page.request.post("/upload", {
    multipart: { file: { name: "moving.gif", mimeType: "image/gif", buffer: gif(150, 90) }, rating: "g", tags: "layout_comment_badge" },
  });
  expect(upload.ok()).toBeTruthy();
  const path = new URL(upload.url()).pathname;
  await page.goto(path);
  await page.getByLabel("Add a comment").fill("It moves.");
  await page.getByRole("button", { name: "Post comment" }).click();
  await expect(page.locator("article.comment")).toContainText("It moves.");

  for (const width of [375, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto(`/comments?post_id=${path.split("/").pop()}`);
    const card = page.locator(".comment-post a.post-card");
    const thumb = card.locator(".thumb");
    const badge = card.locator(".badge");
    await expect(badge).toBeVisible();
    await expect.poll(() => thumb.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0)).toBe(true);
    const cardBox = (await card.boundingBox())!;
    const thumbBox = (await thumb.boundingBox())!;
    const badgeBox = (await badge.boundingBox())!;
    // No panel around the picture, and the badge lies on it.
    expect(cardBox.width).toBeLessThanOrEqual(thumbBox.width + 1);
    expect(cardBox.height).toBeLessThanOrEqual(thumbBox.height + 1);
    expect(badgeBox.x).toBeGreaterThanOrEqual(thumbBox.x);
    expect(badgeBox.y).toBeGreaterThanOrEqual(thumbBox.y);
    expect(badgeBox.x + badgeBox.width).toBeLessThanOrEqual(thumbBox.x + thumbBox.width);
    expect(badgeBox.y + badgeBox.height).toBeLessThanOrEqual(thumbBox.y + thumbBox.height);
  }
});

test("post info sits left of the picture, and below it on phones", async ({ page }) => {
  await authenticateAdmin(page);
  const upload = await page.request.post("/upload", {
    multipart: { file: { name: "side.png", mimeType: "image/png", buffer: png(640, 480) }, rating: "g", tags: "layout_side" },
  });
  expect(upload.ok()).toBeTruthy();
  const path = new URL(upload.url()).pathname;
  for (const width of [375, 900, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto(path);
    await expectPageFits(page);
    const info = (await page.locator(".post-info").boundingBox())!;
    const media = (await page.locator(".post-media").boundingBox())!;
    if (width >= 900) expect(info.x + info.width).toBeLessThanOrEqual(media.x);
    else expect(info.y).toBeGreaterThanOrEqual(media.y + media.height);
  }
});

test("mobile forms remain usable without JavaScript", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 320, height: 800 } });
  const page = await context.newPage();
  try {
    await authenticateAdmin(page);
    await page.goto("/uploads/new");
    await expectPageFits(page);
    await page.goto(await stageUpload(page, png(64, 48)));
    await page.getByText("Artist's commentary", { exact: false }).click();
    await expect(page.locator("#commentary_description")).toBeVisible();
    await expectPageFits(page);
    await page.goto("/tags");
    await page.getByLabel("Name", { exact: true }).fill("nonexistent_layout_tag");
    await page.getByRole("button", { name: "Search", exact: true }).click();
    await expect(page.getByText("No tags found.")).toBeVisible();
    await expectPageFits(page);
  } finally {
    await context.close();
  }
});
