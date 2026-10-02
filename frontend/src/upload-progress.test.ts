import assert from "node:assert/strict";
import { test } from "node:test";

import { nextStep } from "./upload-progress.ts";

const file = (id: number, status: "pending" | "ready" | "failed") => ({ id, status, url: `/uploads/1/assets/${id}` });

test("goes to the first file ready", () => {
  const before = { pending: 2, ready: 0 };
  assert.equal(nextStep({ pending: 2, files: [file(1, "pending"), file(2, "pending")] }, before), null);
  assert.equal(nextStep({ pending: 1, files: [file(1, "pending"), file(2, "ready")] }, before), "/uploads/1/assets/2");
  // A failure alone shows the page again, saying why.
  assert.equal(nextStep({ pending: 1, files: [file(1, "failed"), file(2, "pending")] }, before), "reload");
});

test("shows what finished once one was ready", () => {
  const before = { pending: 1, ready: 1 };
  assert.equal(nextStep({ pending: 1, files: [file(1, "ready"), file(2, "pending")] }, before), null);
  assert.equal(nextStep({ pending: 0, files: [file(1, "ready"), file(2, "ready")] }, before), "reload");
});

test("a file's page waits for that file", () => {
  const before = { pending: 2, ready: 0 };
  const now = { pending: 1, files: [file(1, "ready"), file(2, "pending")] };
  assert.equal(nextStep(now, before, 2), null);
  assert.equal(nextStep(now, before, 1), "reload");
});
