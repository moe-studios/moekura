import assert from "node:assert/strict";
import { test } from "node:test";

import { centred } from "./crop-picker.ts";

test("the square stays inside the picture", () => {
  assert.deepEqual(centred(50, 50, 20, 400, 100), { left: 40, top: 40, side: 20 });
  assert.deepEqual(centred(5, 95, 40, 400, 100), { left: 0, top: 60, side: 40 });
  assert.deepEqual(centred(200, 50, 500, 400, 100), { left: 150, top: 0, side: 100 });
});
