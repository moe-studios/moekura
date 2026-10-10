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

test("questions name what the form holds", () => {
  const fields: Record<string, object> = {
    network: { value: " 203.0.113.0/24 " },
    days: { selectedOptions: [{ text: "1 week" }] },
  };
  const form = {
    getAttribute: () => null,
    querySelector: (selector: string) => fields[/name="(.+)"/.exec(selector)![1]!] ?? null,
  } as unknown as Element;
  assert.equal(
    questionFor(form, element("Ban %network% for %days%? %unknown% stays")),
    "Ban 203.0.113.0/24 for 1 week? %unknown% stays",
  );
});
