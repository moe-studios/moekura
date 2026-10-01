// Messages in the page's language, from the `js-` messages the layout
// puts in `<body data-messages>`. Each call gives the English text too,
// used when the page has no such message (and in tests).

let messages: Record<string, string> | null = null;

function load(): Record<string, string> {
  if (messages === null) {
    try {
      const data = typeof document === "undefined" ? undefined : document.body?.dataset.messages;
      messages = data ? JSON.parse(data) : {};
    } catch {
      messages = {};
    }
  }
  return messages ?? {};
}

/** Message `key` (without its `js-` prefix), with `{$name}`s filled in. */
export function t(key: string, english: string, args: Record<string, string | number> = {}): string {
  let text = load()[`js-${key}`] ?? english;
  for (const [name, value] of Object.entries(args)) {
    text = text.replaceAll(`{$${name}}`, String(value));
  }
  return text;
}
