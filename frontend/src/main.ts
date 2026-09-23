// Progressive enhancements. Every page works without this script; it only
// makes things quicker to use.

import { attachAll } from "./autocomplete.ts";

// Lets styles tell whether scripts run.
document.documentElement.classList.add("js");

attachAll();
