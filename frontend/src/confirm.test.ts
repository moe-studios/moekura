import assert from "node:assert/strict";
import { test } from "node:test";

import { questionFor } from "./confirm.ts";

/** Just enough of an element for `questionFor`. */
function element(confirm?: string): Element {
  return { getAttribute: (name: string) => (name === "data-confirm" ? (confirm ?? null) : null) } as Element;
}

test("the button's question wins over the form's", () => {
  assert.equal(questionFor(element("Delete the group?"), element("Revoke it?")), "Revoke it?");
  assert.equal(questionFor(element("Delete the group?"), element()), "Delete the group?");
  assert.equal(questionFor(element("Delete the group?"), null), "Delete the group?");
});

test("forms without a question submit as usual", () => {
  assert.equal(questionFor(element(), element()), null);
  assert.equal(questionFor(element(), null), null);
});
