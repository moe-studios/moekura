import assert from "node:assert/strict";
import { test } from "node:test";

import { parseScript, postId } from "./tag-script.ts";

test("scripts add, remove and rate", () => {
  assert.deepEqual(parseScript("  cat  -dog rating:S artist:someone "), {
    add: ["cat", "artist:someone"],
    remove: ["dog"],
    rating: "s",
  });
  assert.deepEqual(parseScript("rating:explicit"), { add: [], remove: [], rating: "e" });
  assert.throws(() => parseScript("rating:x"), /Unknown rating/);
});

test("post ids come from card links", () => {
  assert.equal(postId("/posts/123?q=cat"), "123");
  assert.equal(postId("/pools/4"), null);
});
