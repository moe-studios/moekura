import assert from "node:assert/strict";
import { test } from "node:test";

import { asLink } from "./upload.ts";

test("takes web links", () => {
  assert.equal(asLink("https://www.pixiv.net/artworks/1"), "https://www.pixiv.net/artworks/1");
  assert.equal(asLink("  http://example.com/a.png\n"), "http://example.com/a.png");
});

test("adds https to links without a scheme", () => {
  assert.equal(asLink("x.com/artist/status/1"), "https://x.com/artist/status/1");
  assert.equal(asLink("example.com"), "https://example.com/");
});

test("leaves other text alone", () => {
  assert.equal(asLink(""), null);
  assert.equal(asLink("long_hair"), null);
  assert.equal(asLink("cat dog.png"), null);
  assert.equal(asLink("javascript:alert(1)"), null);
  assert.equal(asLink("file:///etc/passwd"), null);
  assert.equal(asLink("mailto:a@example.com"), null);
});
