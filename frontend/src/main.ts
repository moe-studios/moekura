// Progressive enhancements. Every page works without this script; it only
// makes things quicker to use.

import { attachAll } from "./autocomplete.ts";
import { enableShortcuts } from "./keyboard.ts";
import { enablePoolOrder } from "./pool-order.ts";
import { enhanceReactions } from "./reactions.ts";
import { enableReader } from "./reader.ts";

// Lets styles tell whether scripts run.
document.documentElement.classList.add("js");

attachAll();
enhanceReactions();
enableShortcuts();
enablePoolOrder();
enableReader();
