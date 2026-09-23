import assert from "node:assert/strict";
import { test } from "node:test";

import { target } from "./autocomplete.ts";

/** `target` with the caret at `|` in `text`. */
function at(text: string, mode: "search" | "tags" = "search") {
  const caret = text.indexOf("|");
  return target(text.replace("|", ""), caret, mode);
}

test("completes the word under the caret", () => {
  assert.deepEqual(at("cat long_h|"), { start: 4, end: 10, typed: "long_h", kind: "tag" });
  // Only the part before the caret counts as typed; the rest is replaced.
  assert.deepEqual(at("lo|ng_hair cat"), { start: 0, end: 9, typed: "lo", kind: "tag" });
  assert.equal(at("cat |"), null);
  assert.equal(at("long_*|"), null);
});

test("keeps search prefixes", () => {
  assert.deepEqual(at("-lo|"), { start: 1, end: 3, typed: "lo", kind: "tag" });
  assert.deepEqual(at("~lo|"), { start: 1, end: 3, typed: "lo", kind: "tag" });
  // In tag fields a leading "-" is part of what was typed.
  assert.deepEqual(at("-lo|", "tags"), { start: 0, end: 3, typed: "-lo", kind: "tag" });
});

test("metatag values", () => {
  assert.deepEqual(at("rating:q|"), {
    start: 7,
    end: 8,
    typed: "q",
    kind: "metatag-value",
    metatag: "rating",
  });
  assert.deepEqual(at("-filetype:png,we|"), {
    start: 14,
    end: 16,
    typed: "we",
    kind: "metatag-value",
    metatag: "filetype",
  });
  // Metatags are search syntax only.
  assert.deepEqual(at("rating:q|", "tags"), { start: 0, end: 8, typed: "rating:q", kind: "tag" });
});

test("category prefixes", () => {
  assert.deepEqual(at("artist:some|", "tags"), { start: 7, end: 11, typed: "some", kind: "tag" });
  assert.equal(at("artist:|", "tags"), null);
  // Other colons belong to the tag.
  assert.deepEqual(at("re:ze|", "tags"), { start: 0, end: 5, typed: "re:ze", kind: "tag" });
});
