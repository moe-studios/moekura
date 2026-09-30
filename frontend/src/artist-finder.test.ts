import assert from "node:assert/strict";
import { test } from "node:test";

import { firstUrl } from "./artist-finder.ts";

test("the first web address", () => {
  assert.equal(firstUrl(["", " https://x.com/a/status/1 ", "https://b"]), "https://x.com/a/status/1");
  assert.equal(firstUrl(["not a url", "ftp://a"]), null);
});
