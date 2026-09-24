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

/** A solid-colour PNG; a random colour, so reruns don't upload duplicates. */
export function png(width = 32, height = 32): Buffer {
  const [r, g, b] = [0, 0, 0].map(() => Math.floor(Math.random() * 256));
  const row = Buffer.alloc(1 + width * 3);
  for (let x = 0; x < width; x++) row.set([r, g, b], 1 + x * 3);
  const pixels = Buffer.concat(Array.from({ length: height }, () => row));
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
