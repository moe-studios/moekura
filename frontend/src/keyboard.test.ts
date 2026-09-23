import assert from "node:assert/strict";
import { test } from "node:test";

import { SHORTCUTS, actionFor } from "./keyboard.ts";

test("keys map to actions", () => {
  assert.equal(actionFor("a"), "prev");
  assert.equal(actionFor("ArrowRight"), "next");
  assert.equal(actionFor("e"), "edit");
  assert.equal(actionFor("x"), null);
  // Upper case (shift held) isn't a shortcut, so capital letters type.
  assert.equal(actionFor("D"), null);
});

test("every documented key does something", () => {
  for (const [keys] of SHORTCUTS) {
    for (const key of keys.split(", ")) {
      const name = key === "←" ? "ArrowLeft" : key === "→" ? "ArrowRight" : key;
      assert.notEqual(actionFor(name), null, key);
    }
  }
});
