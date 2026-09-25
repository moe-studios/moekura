import assert from "node:assert/strict";
import { test } from "node:test";

import { reorder } from "./pool-order.ts";

test("moves an id before another", () => {
  assert.deepEqual(reorder(["1", "2", "3", "4"], "4", "2"), ["1", "4", "2", "3"]);
  assert.deepEqual(reorder(["1", "2", "3"], "1", "3"), ["2", "1", "3"]);
});

test("moves an id to the end", () => {
  assert.deepEqual(reorder(["1", "2", "3"], "1", null), ["2", "3", "1"]);
  // An unknown neighbour counts as the end.
  assert.deepEqual(reorder(["1", "2"], "1", "9"), ["2", "1"]);
});
