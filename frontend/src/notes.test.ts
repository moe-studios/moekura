import assert from "node:assert/strict";
import { test } from "node:test";

import { popupPosition } from "./notes.ts";

test("popups sit below their box, inside the image", () => {
  assert.deepEqual(popupPosition({ left: 10, top: 20, width: 50, height: 30 }, 500, 200), { left: 10, top: 54 });
  // Near the right edge, it moves left.
  assert.deepEqual(popupPosition({ left: 450, top: 0, width: 40, height: 10 }, 500, 200), { left: 300, top: 14 });
  // Wider than the image: from the left edge.
  assert.deepEqual(popupPosition({ left: 50, top: 0, width: 5, height: 5 }, 100, 200), { left: 0, top: 9 });
});
