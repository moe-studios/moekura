import { deflateSync, crc32 } from "node:zlib";
import { expect, type Page } from "@playwright/test";

export const admin = {
  name: process.env.E2E_ADMIN_NAME ?? "boss",
  password: process.env.E2E_ADMIN_PASSWORD ?? "e2e admin password",
};

/** Logs in through the form. */
export async function logIn(page: Page, name: string, password: string): Promise<void> {
  await page.goto("/login");
  await page.getByLabel("Name").fill(name);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Log in" }).click();
  await expect(page.getByText("Welcome back!")).toBeVisible();
}

/**
 * A PNG of random 8×8 blocks, so reruns don't upload duplicates and no two
 * look alike (solid colours would all share a perceptual hash, and the
 * upload form would warn about look-alikes).
 */
export function png(width = 32, height = 32): Buffer {
  const block = 8;
  const colours = Array.from({ length: Math.ceil(width / block) * Math.ceil(height / block) }, () =>
    [0, 0, 0].map(() => Math.floor(Math.random() * 256)),
  );
  const rows = Array.from({ length: height }, (_, y) => {
    const row = Buffer.alloc(1 + width * 3);
    for (let x = 0; x < width; x++) {
      row.set(colours[Math.floor(y / block) * Math.ceil(width / block) + Math.floor(x / block)], 1 + x * 3);
    }
    return row;
  });
  const pixels = Buffer.concat(rows);
  const chunk = (type: string, data: Buffer) => {
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8); // 8-bit RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(pixels)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/**
 * Sends `file` from the upload form (choosing it uploads it), then posts it
 * from its page with `tags` and a general rating. Returns the post's path.
 */
export async function uploadPost(page: Page, file: { name: string; buffer: Buffer }, tags: string): Promise<string> {
  await page.goto("/uploads/new");
  await page.locator("#file").setInputFiles({ ...file, mimeType: "image/png" });
  await expect(page).toHaveURL(/\/uploads\/\d+$/);
  await page.locator("input[name=rating][value=g]").check();
  await page.locator("#tags").fill(tags);
  await page.getByRole("button", { name: "Post", exact: true }).click();
  await expect(page).toHaveURL(/\/posts\/\d+(\?check=1.*)?$/);
  return new URL(page.url()).pathname;
}

/** Stages `buffer` as an upload without scripts; returns the upload's path. */
export async function stageUpload(page: Page, buffer: Buffer): Promise<string> {
  const response = await page.request.post("/uploads", {
    multipart: { file: { name: "staged.png", mimeType: "image/png", buffer } },
  });
  expect(response.ok()).toBeTruthy();
  return new URL(response.url()).pathname;
}
