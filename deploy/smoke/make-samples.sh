#!/bin/sh
# Makes one file of every type uwubooru accepts, in /samples, with Debian's
# full ffmpeg and libvips. Run in a throwaway debian:trixie container; see
# check.sh.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends ffmpeg libvips-tools \
    libheif-plugin-aomenc libheif-plugin-dav1d >/dev/null
cd /samples
make() {
    source=$1
    shift
    ffmpeg -hide_banner -loglevel error -y -f lavfi -i "$source" "$@"
}
still=testsrc2=size=640x480:duration=1
make "$still" -frames:v 1 still.png
make "$still" -frames:v 1 still.jpg
vips copy still.png still.webp
vips copy still.png still.avif
vips copy still.png still.jxl
make testsrc2=size=160x120:duration=0.5:rate=6 -loop 0 anim.gif
video=testsrc2=size=320x240:duration=2
make "$video" -c:v libx264 -pix_fmt yuv420p h264.mp4
make "$video" -c:v libvpx vp8.webm
make "$video" -c:v libvpx-vp9 vp9.webm
make "$video" -c:v libaom-av1 -cpu-used 8 av1.mp4
ls -l
