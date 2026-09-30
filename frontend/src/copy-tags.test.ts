import assert from "node:assert/strict";
import { test } from "node:test";

import { withTags } from "./copy-tags.ts";

test("adds the tags missing from the box", () => {
  assert.equal(withTags("cat dog", "dog long_hair  solo"), "cat dog long_hair solo ");
  assert.equal(withTags("", "cat"), "cat ");
  assert.equal(withTags("cat", ""), "cat");
});
