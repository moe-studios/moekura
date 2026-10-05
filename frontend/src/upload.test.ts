import assert from "node:assert/strict";
import { test } from "node:test";

import { asLink, isUploadForm, sendsNow } from "./upload.ts";

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

test("only the upload form, posting to this site's /uploads, is sent by itself", () => {
  const page = "https://booru.example/uploads/new?url=x";
  assert.equal(isUploadForm("/uploads", "post", page), true);
  assert.equal(isUploadForm("https://booru.example/uploads", "POST", page), true);
  // Forms slipped into a page, posting anywhere else.
  assert.equal(isUploadForm("/comments", "post", page), false);
  assert.equal(isUploadForm("/uploads?x=1", "post", page), false);
  assert.equal(isUploadForm("/uploads/1/assets/2", "post", page), false);
  assert.equal(isUploadForm("https://elsewhere.example/uploads", "post", page), false);
  assert.equal(isUploadForm("//elsewhere.example/uploads", "post", page), false);
  assert.equal(isUploadForm("/uploads", "get", page), false);
  assert.equal(isUploadForm("/uploads", null, page), false);
  assert.equal(isUploadForm(null, "post", page), false);
});

test("only the upload page sends a link given with it", () => {
  assert.equal(sendsNow("", "https://booru.example/uploads/new?url=x&token=t"), true);
  assert.equal(sendsNow(null, "https://booru.example/uploads/new?url=x"), false);
  assert.equal(sendsNow("", "https://booru.example/artists/1"), false);
  assert.equal(sendsNow("", "https://booru.example/uploads"), false);
});
