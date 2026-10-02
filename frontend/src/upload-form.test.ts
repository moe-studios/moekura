import assert from "node:assert/strict";
import { test } from "node:test";

import { asDock, tagCount } from "./upload-form.ts";

test("counts the tags a tags box adds", () => {
  assert.equal(tagCount(""), 0);
  assert.equal(tagCount("  cat dog\nartist:someone "), 3);
  assert.equal(tagCount("cat -dog rating:s source:https://x.com/a pool:5 parent:none"), 1);
  assert.equal(tagCount(":) cat"), 2);
});

test("knows the dock positions", () => {
  assert.equal(asDock("left"), "left");
  assert.equal(asDock("bottom"), "bottom");
  assert.equal(asDock("top"), null);
  assert.equal(asDock(null), null);
});
