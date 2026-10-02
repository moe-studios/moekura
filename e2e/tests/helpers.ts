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
 * A two-frame animated GIF of random colours (so reruns don't upload
 * duplicates), each frame one colour. The pixels are written uncompressed:
 * a clear code before every second pixel keeps the codes 3 bits wide.
 */
export function gif(width = 32, height = 32): Buffer {
  const palette = Buffer.from(Array.from({ length: 12 }, () => Math.floor(Math.random() * 256)));
  const header = Buffer.alloc(13);
  header.write("GIF89a", 0, "ascii");
  header.writeUInt16LE(width, 6);
  header.writeUInt16LE(height, 8);
  header.set([0xf1, 0, 0], 10); // a global table of 4 colours
  const loop = Buffer.from([0x21, 0xff, 0x0b, ...Buffer.from("NETSCAPE2.0", "ascii"), 0x03, 0x01, 0, 0, 0]);
  const frame = (colour: number) => {
    const codes: number[] = [];
    for (let i = 0; i < width * height; i++) {
      if (i % 2 === 0) codes.push(4);
      codes.push(colour);
    }
    codes.push(5);
    const bytes: number[] = [];
    let bits = 0;
    let filled = 0;
    for (const code of codes) {
      bits |= code << filled;
      filled += 3;
      while (filled >= 8) {
        bytes.push(bits & 0xff);
        bits >>= 8;
        filled -= 8;
      }
    }
    if (filled > 0) bytes.push(bits);
    const blocks: number[] = [];
    for (let i = 0; i < bytes.length; i += 255) {
      const block = bytes.slice(i, i + 255);
      blocks.push(block.length, ...block);
    }
    const descriptor = Buffer.alloc(10);
    descriptor[0] = 0x2c;
    descriptor.writeUInt16LE(width, 5);
    descriptor.writeUInt16LE(height, 7);
    return Buffer.from([
      0x21, 0xf9, 0x04, 0x00, 50, 0, 0, 0, // 0.5 s per frame
      ...descriptor, 2, ...blocks, 0,
    ]);
  };
  return Buffer.concat([header, palette, loop, frame(0), frame(1), Buffer.from([0x3b])]);
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
