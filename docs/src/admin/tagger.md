# The tagger

The tagger suggests tags and a rating for new uploads with a machine
learning model: one of SmilingWolf's WD taggers, trained on Danbooru and
run on the CPU with ONNX Runtime. Suggestions appear under **Edit** on each
post (see [Tags](../using/tags.md#suggestions-from-the-tagger)), and
`ai:tag` searches for posts where a tag is suggested but not applied. The
tagger can also apply what it's surest of by itself.

It is optional, and a process of its own, `moekura tagger`: it takes work
from the job queue, so it can run on the same machine as the site or on a
bigger one elsewhere, as long as it reaches the database and the file
storage.

## Setting it up

With Docker Compose, add the tagger to the tiny setup:

```sh
docker compose -f deploy/compose.tiny.yml -f deploy/compose.tagger.yml up -d
```

That runs the `-tagger` image (`ghcr.io/moe-studios/moekura:<version>-tagger`,
the app plus ONNX Runtime, for amd64 and arm64) as a second service, and
turns `tagger.enabled` on for both. On first start the tagger downloads its
model into the `models` volume, checks it against its SHA-256 checksum, and
starts on the queue.

Elsewhere, it takes three things:

1. A build with the tagger: the release binaries and images have it;
   from source, `cargo build --release -p moekura --features tagger`.
2. ONNX Runtime 1.17 or newer: Microsoft's
   [`onnxruntime-linux-x64` or `-aarch64`](https://github.com/microsoft/onnxruntime/releases)
   archives have `libonnxruntime.so`. Point `tagger.runtime` (or
   `ORT_DYLIB_PATH`) at it, or put it on the library path.
3. `enabled = true` in `[tagger]`, for **every** process: `serve` and
   `worker` queue each processed upload for the tagger, and `moekura
   tagger` does the work.

```toml
[tagger]
enabled = true
runtime = "/opt/onnxruntime/lib/libonnxruntime.so"
```

`moekura tagger --check` loads ONNX Runtime and says where from, without
touching the database. Posts uploaded before the tagger was set up are
queued with

```sh
moekura admin tag-backlog              # posts it hasn't seen, oldest first
moekura admin tag-backlog --all        # every post, e.g. after changing models
moekura admin tag-backlog --limit 1000
```

## What it suggests

**Admin → Settings** has the tagger's settings:

- **Thresholds**, per tag category: how sure the model must be, in
  percent, to suggest a tag. The defaults, 35% for general tags and 85%
  for characters, follow what the models' authors recommend; categories
  left blank use General's. Suggestions below the thresholds at the time a
  post is tagged aren't kept, so lowering them affects posts tagged
  afterwards (or requeued with `tag-backlog --all`); raising them hides
  suggestions at once.
- **Applying suggestions itself**, when the tagger is at least a given
  percent sure (95% by default), and optionally **the rating** too. These
  edits are made by the account `tagger.account` (`tagger` by default),
  so they show in each post's history like anyone's and can be reverted.
  The account is created on first use without a password, so nobody can
  log in as it; if the name belongs to an account that has one, the
  tagger refuses to use it.

The model's tag names are Danbooru's. They're matched to the site's tags
through aliases; tags the site doesn't have yet are created, in the
model's category (general or character), when first suggested. Deprecated
tags aren't suggested.

## Choosing a model

`tagger.model` picks one of the v3 WD taggers, which share a list of
about 10,800 tags:

| Model | Download | Notes |
|---|---|---|
| `wd-vit-tagger-v3` (default) | 380 MB | the fastest; fine on small machines |
| `wd-convnext-tagger-v3` | 400 MB | similar size and accuracy, somewhat slower |
| `wd-swinv2-tagger-v3` | 470 MB | a little more accurate, slower |
| `wd-eva02-large-tagger-v3` | 1.3 GB | the most accurate, several times slower and larger in memory; for a desktop-class machine |

Each is pinned to one revision with its checksum. Any other model shaped
like these (one input of square BGR images, `[batch, size, size, 3]`, and
one score per line of a `selected_tags.csv`) works with `model = "custom"`
and `model_url`, `model_sha256`, `tags_url` and `tags_sha256`. After
changing models, `moekura admin tag-backlog --all` retags existing posts.

## Hardware

The tagger needs no GPU. What it takes, with the default model:

- **Memory**: about 600 MB once the model is loaded, and about 750 MB
  while tagging. Plan for 1 GB on top of the site. EVA02-Large, going by its
  size, takes over three times as much.
- **Disk**: the model, 380 MB, in `tagger.model_dir`.
- **Time**: measured on a desktop (AMD Ryzen 7 7800X3D), from taking the
  job to saving the suggestions:

  | Threads (`tagger.threads`) | Per post |
  |---|---|
  | 16 (`0`, all of them) | 0.35 s |
  | 4 | 0.46 s |
  | 1 | 1.35 s |

  About 0.15 s of that is preparing the image, the rest running the model.

On a **Raspberry Pi 4** (4 GB or more; 2 GB is too tight next to
PostgreSQL and the site), expect several seconds per post with the default
model, very roughly 3 to 6: its four Cortex-A72 cores have a fraction of a
desktop's speed at this work. That's an estimate, not a measurement; the
tagger logs how long each post took, so check yours. It keeps up with a
small site's uploads easily, and a large backlog takes hours rather than
minutes. Don't run EVA02-Large on one.

`tagger.threads` is threads per post; the default uses every core, which
is right for a machine of its own. Next to the site on a small machine,
leave a core or two free for it. `tagger.workers` tags several posts at
once, each with the whole model; one is usually best.

## Keeping an eye on it

Queued `ml.tag_post` jobs show on **Admin → Overview**. Only a tagger takes
them, so if they pile up, it isn't running. A post the tagger hasn't seen
says so under **Edit**. The tagger logs each post it tags, with how many
suggestions it saved and whether it applied any.
