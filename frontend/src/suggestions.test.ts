import assert from "node:assert/strict";
import { test } from "node:test";

import { withTag } from "./suggestions.ts";

test("adds a tag at the end", () => {
  assert.equal(withTag("cat dog", "long_hair"), "cat dog long_hair ");
  assert.equal(withTag("cat  \n", "dog"), "cat dog ");
  assert.equal(withTag("", "cat"), "cat ");
});

test("leaves tags already there alone", () => {
  assert.equal(withTag("cat dog", "cat"), "cat dog");
  assert.equal(withTag("cat\ndog ", "dog"), "cat\ndog ");
});
