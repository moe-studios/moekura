// Progressive enhancements. Every page works without this script; it only
// makes things quicker to use.

import { attachAll } from "./autocomplete.ts";
import { enableAutosubmit } from "./autosubmit.ts";
import { enableConfirm } from "./confirm.ts";
import { enableCopyTags } from "./copy-tags.ts";
import { enableShortcuts } from "./keyboard.ts";
import { enableLayout } from "./layout.ts";
import { enableNoteEditor } from "./note-editor.ts";
import { enableNotes } from "./notes.ts";
import { enablePoolOrder } from "./pool-order.ts";
import { enhanceReactions } from "./reactions.ts";
import { enableReader } from "./reader.ts";
import { enableSelectAll } from "./select-all.ts";
import { enableSuggestions } from "./suggestions.ts";
import { enableTagScript } from "./tag-script.ts";
import { enableToasts } from "./toast.ts";
import { enableUpload } from "./upload.ts";

// Lets styles tell whether scripts run.
document.documentElement.classList.add("js");

enableToasts();
enableConfirm();
enableAutosubmit();
enableLayout();
attachAll();
enhanceReactions();
enableShortcuts();
enablePoolOrder();
enableReader();
enableNotes();
enableNoteEditor();
enableTagScript();
enableSuggestions();
enableCopyTags();
enableSelectAll();
enableUpload();
