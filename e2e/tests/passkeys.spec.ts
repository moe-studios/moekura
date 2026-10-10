import { createHmac } from "node:crypto";
import { expect, test, type Page } from "@playwright/test";

// Unique per run, so the tests also work against a site used before.
const run = Date.now().toString(36);
const member = { name: `e2e_pk_${run}`, password: "correct horse battery" };

/**
 * Gives `page` a virtual platform authenticator (Chromium's, through the
 * DevTools protocol), like a laptop's fingerprint reader: it keeps
 * discoverable passkeys and verifies the user without asking.
 */
async function addAuthenticator(page: Page): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}

/** The authenticator app's current code for a base32 `secret` (RFC 6238). */
function totp(secret: string): string {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of secret.replace(/[\s=]/g, "").toUpperCase()) bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  const key = Buffer.from((bits.match(/.{8}/g) ?? []).map((b) => parseInt(b, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 1000 / 30)));
  const mac = createHmac("sha1", key).update(counter).digest();
  const offset = mac[mac.length - 1]! & 0xf;
  return ((mac.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).toString().padStart(6, "0");
}

async function logInWithPassword(page: Page): Promise<void> {
  await page.goto("/login");
  await page.getByLabel("Name").fill(member.name);
  await page.getByLabel("Password").fill(member.password);
  await page.getByRole("button", { name: "Log in", exact: true }).click();
}

test("add a passkey, log in with it, and remove it", async ({ page }) => {
  await addAuthenticator(page);

  await test.step("register", async () => {
    await page.goto("/register");
    await page.getByLabel("Name").fill(member.name);
    await page.getByLabel("Password", { exact: true }).fill(member.password);
    await page.getByLabel("Repeat password").fill(member.password);
    await page.getByRole("button", { name: "Register" }).click();
    await expect(page.getByText("Your account is ready")).toBeVisible();
  });

  await test.step("add a passkey on the account page", async () => {
    await page.goto("/settings/account");
    const section = page.locator("#passkeys");
    await expect(section.getByText("You haven't added a passkey.")).toBeVisible();
    await section.getByLabel("Name").fill("Laptop");
    await section.getByLabel("Your password").fill("not my password");
    await section.getByRole("button", { name: "Add a passkey" }).click();
    await expect(section.getByRole("alert")).toHaveText("Wrong password.");
    await section.getByLabel("Your password").fill(member.password);
    await section.getByRole("button", { name: "Add a passkey" }).click();
    await expect(page.getByText("Saved.")).toBeVisible();
    await expect(page.locator("#passkeys table")).toContainText("Laptop");
    await expect(page.locator("#passkeys table")).toContainText("never");
  });

  await test.step("log in with it from the name field's suggestions", async () => {
    // The virtual authenticator answers the login page's request for
    // suggestions at once, as if the passkey had been picked there.
    await page.context().clearCookies();
    await page.goto("/login?next=%2Fsettings%2Faccount");
    await expect(page.getByText("Welcome back!")).toBeVisible();
    await expect(page).toHaveURL(/\/settings\/account$/);
    await expect(page.locator("#passkeys table")).not.toContainText("never");
  });

  await test.step("log in with it with the button", async () => {
    // From here on, as in a browser that can't suggest passkeys.
    await page.addInitScript(() => {
      PublicKeyCredential.isConditionalMediationAvailable = async () => false;
    });
    await page.context().clearCookies();
    await page.goto("/login?next=%2Fsettings%2Faccount");
    await page.getByRole("button", { name: "Log in with a passkey" }).click();
    await expect(page.getByText("Welcome back!")).toBeVisible();
    await expect(page).toHaveURL(/\/settings\/account$/);
  });

  await test.step("use it instead of a two-factor code", async () => {
    await page.goto("/settings/two-factor");
    await page.getByLabel("Your password").fill(member.password);
    await page.getByRole("button", { name: "Set it up" }).click();
    const secret = await page.locator("code.secret").textContent();
    await page.getByLabel("Code", { exact: true }).fill(totp(secret ?? ""));
    await page.getByRole("button", { name: "Turn on" }).click();
    await page.getByRole("link", { name: "I've saved them" }).click();

    await page.context().clearCookies();
    await logInWithPassword(page);
    await expect(page).toHaveURL(/\/login\/code$/);
    await page.getByRole("button", { name: "Use a passkey instead" }).click();
    await expect(page.getByText("Welcome back!")).toBeVisible();
  });

  await test.step("rename and remove it", async () => {
    await page.goto("/settings/account");
    const row = page.locator("#passkeys tbody tr").first();
    await row.locator("summary", { hasText: "Rename" }).click();
    await row.getByLabel("Name").fill("Work laptop");
    await row.getByRole("button", { name: "Rename" }).click();
    await expect(page.locator("#passkeys table")).toContainText("Work laptop");

    await row.locator("summary", { hasText: "Remove" }).click();
    await row.getByLabel("Your password").fill(member.password);
    page.once("dialog", (dialog) => void dialog.accept());
    await row.getByRole("button", { name: "Remove" }).click();
    await expect(page.locator("#passkeys").getByText("You haven't added a passkey.")).toBeVisible();
  });

  await test.step("a removed passkey doesn't log in", async () => {
    await page.context().clearCookies();
    await page.goto("/login");
    await page.getByRole("button", { name: "Log in with a passkey" }).click();
    await expect(page.locator(".passkey-login").getByRole("alert")).toContainText("isn't registered here");
  });
});
