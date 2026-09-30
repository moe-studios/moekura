import assert from "node:assert/strict";
import { test } from "node:test";

import { chosenTag, withoutTag } from "./related-tags.ts";

test("the tag under the caret", () => {
  assert.equal(chosenTag("cat Long_hair", 6), "long_hair");
  assert.equal(chosenTag("cat long_hair", 3), "cat");
  assert.equal(chosenTag("cat ", 4), null);
  assert.equal(chosenTag("rating:e", 3), null);
  assert.equal(chosenTag("-dog", 2), null);
});

test("takes a tag out", () => {
  assert.equal(withoutTag("cat dog  cute", "dog"), "cat cute ");
  assert.equal(withoutTag("Cat", "cat"), "");
});
