// Progressive enhancements. Every page works without this script; it only
// makes things quicker to use.

import { enableArtistFinder } from "./artist-finder.ts";
import { attachAll } from "./autocomplete.ts";
import { enableAutosubmit } from "./autosubmit.ts";
import { enableClipboard } from "./clipboard.ts";
import { enableConfirm } from "./confirm.ts";
import { enableCopyTags } from "./copy-tags.ts";
import { enableShortcuts } from "./keyboard.ts";
import { enableLayout } from "./layout.ts";
import { enableNoteEditor } from "./note-editor.ts";
import { enableNotes } from "./notes.ts";
import { enablePasskeys } from "./passkeys.ts";
import { enablePoolOrder } from "./pool-order.ts";
import { enhanceReactions } from "./reactions.ts";
import { enableReader } from "./reader.ts";
import { enableRelatedTags } from "./related-tags.ts";
import { enableResized } from "./resized.ts";
import { enableSelectAll } from "./select-all.ts";
import { enableSourceData } from "./source-data.ts";
import { enableSuggestions } from "./suggestions.ts";
import { enableTagScript } from "./tag-script.ts";
import { enableToasts } from "./toast.ts";
import { enableUpload } from "./upload.ts";
import { enableUploadForm } from "./upload-form.ts";
import { enableUploadProgress } from "./upload-progress.ts";

// Lets styles tell whether scripts run.
document.documentElement.classList.add("js");

// What the user turned off in their settings.
const off = (feature: string) => document.documentElement.dataset[feature] === "off";

enableToasts();
enableConfirm();
enableAutosubmit();
enableLayout();
if (!off("autocomplete")) attachAll();
enhanceReactions();
if (!off("shortcuts")) enableShortcuts();
enablePoolOrder();
enableReader();
enableResized();
enableNotes();
enableNoteEditor();
enableTagScript();
enableSuggestions();
enableCopyTags();
enableRelatedTags();
enableSelectAll();
enableUpload();
enableUploadForm();
enableUploadProgress();
enableArtistFinder();
enableClipboard();
enableSourceData();
enablePasskeys();
