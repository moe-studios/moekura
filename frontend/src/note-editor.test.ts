import assert from "node:assert/strict";
import { test } from "node:test";

import { boxBetween, moved, resized } from "./note-editor.ts";

test("boxes are drawn from any corner, inside the image", () => {
  assert.deepEqual(boxBetween({ x: 30, y: 40 }, { x: 10.4, y: 5.6 }, 100, 100), { x: 10, y: 6, width: 20, height: 34 });
  assert.deepEqual(boxBetween({ x: -10, y: 90 }, { x: 20, y: 150 }, 100, 100), { x: 0, y: 90, width: 20, height: 10 });
});

test("moving keeps the box inside the image", () => {
  const box = { x: 10, y: 10, width: 20, height: 20 };
  assert.deepEqual(moved(box, 5, -3, 100, 100), { x: 15, y: 7, width: 20, height: 20 });
  assert.deepEqual(moved(box, 500, -50, 100, 100), { x: 80, y: 0, width: 20, height: 20 });
});

test("resizing keeps at least a pixel, inside the image", () => {
  const box = { x: 10, y: 10, width: 20, height: 20 };
  assert.deepEqual(resized(box, 5, 5, 100, 100), { x: 10, y: 10, width: 25, height: 25 });
  assert.deepEqual(resized(box, -50, 500, 100, 100), { x: 10, y: 10, width: 1, height: 90 });
});
