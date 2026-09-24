#!/bin/sh
# Runs the media tools in the uwubooru image the way the app does, on the
# samples make-samples.sh made:
#
#   docker run --rm -v "$PWD/deploy/smoke:/smoke:ro" -v samples:/samples \
#       debian:trixie sh /smoke/make-samples.sh
#   docker run --rm --entrypoint sh -v "$PWD/deploy/smoke:/smoke:ro" \
#       -v samples:/samples:ro uwubooru /smoke/check.sh
set -eu
cd /samples
out=$(mktemp -d)
export VIPS_CONCURRENCY=1

for tool in vips vipsheader vipsthumbnail; do "$tool" --version >/dev/null; done
ffmpeg -version >/dev/null
ffprobe -version >/dev/null

# Images, with the loaders libvips doesn't trust for hostile files blocked.
for file in still.jpg still.png still.webp still.avif anim.gif; do
    VIPS_BLOCK_UNTRUSTED=1 vipsheader -a "$file" >/dev/null
    VIPS_BLOCK_UNTRUSTED=1 vipsthumbnail "$file" --size '250x250>' -o "$out/thumb.webp[Q=80,keep=none]"
    VIPS_BLOCK_UNTRUSTED=1 vipsthumbnail "$file" --size '250x250>' -o "$out/thumb.avif[Q=60,keep=none]"
    VIPS_BLOCK_UNTRUSTED=1 vipsthumbnail "$file" --size '32x32!' -o "$out/phash.pgm"
    [ "$(vipsheader -f width "$out/thumb.webp")" -le 250 ]
    echo "ok  $file"
done
[ "$(VIPS_BLOCK_UNTRUSTED=1 vipsheader -f n-pages anim.gif)" -gt 1 ]
# JPEG XL is opt-in: its loader runs unblocked.
vipsheader -a still.jxl >/dev/null
vipsthumbnail still.jxl --size '250x250>' -o "$out/thumb.webp[Q=80,keep=none]"
echo "ok  still.jxl"

# Videos: probing, and a poster frame that thumbnails like an image.
for file in h264.mp4 vp8.webm vp9.webm av1.mp4; do
    ffprobe -v error -print_format json -show_streams -show_format "$file" > "$out/probe.json"
    grep -q '"codec_type": "video"' "$out/probe.json"
    ffmpeg -hide_banner -loglevel error -y -ss 1.000 -i "$file" -frames:v 1 -an "$out/poster.png"
    [ "$(vipsheader -f width "$out/poster.png")" -eq 320 ]
    VIPS_BLOCK_UNTRUSTED=1 vipsthumbnail "$out/poster.png" --size '250x250>' -o "$out/thumb.webp[Q=80,keep=none]"
    echo "ok  $file"
done
echo "all media checks passed"
