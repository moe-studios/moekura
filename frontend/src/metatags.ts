// Search metatags and their fixed values, mirroring moekura_core::search.
// A Rust test (crates/web/src/tags.rs) checks every metatag and order
// name appears here.

export const METATAGS: Readonly<Record<string, readonly string[]>> = {
  id: [],
  rating: ["general", "sensitive", "questionable", "explicit"],
  status: ["pending", "active", "flagged", "deleted", "any"],
  user: [],
  score: [],
  favcount: [],
  commentcount: [],
  width: [],
  height: [],
  mpixels: [],
  ratio: [],
  filesize: [],
  duration: [],
  date: [],
  filetype: ["jpg", "png", "gif", "webp", "avif", "jxl", "mp4", "webm"],
  md5: [],
  parent: ["none", "any"],
  tagcount: [],
  order: [
    "id",
    "id_asc",
    "score",
    "score_asc",
    "favcount",
    "favcount_asc",
    "mpixels",
    "mpixels_asc",
    "filesize",
    "filesize_asc",
    "landscape",
    "portrait",
    "duration",
    "duration_asc",
    "tagcount",
    "tagcount_asc",
    "random",
    "comment",
    "comment_asc",
  ],
  limit: [],
  fav: [],
  ordfav: [],
  similar: [],
};

/// Prefixes that set a new tag's category when tagging.
export const CATEGORIES: readonly string[] = ["artist", "copyright", "character", "general", "meta"];
