# Posts and files

## File metadata

When a file is processed, its metadata is read and kept: the image's
make-up (colour components, bit depth, colour space, frames), EXIF (the
camera, the software, comments), XMP (title, creator tool, keywords), PNG
text chunks, and for videos the container and each stream (codec, frame
rate, bit rate, audio channels). **Metadata**, under the picture on a
post's page, lists it by group, with names like exiftool's: `EXIF:Make`,
`PNG:Software`, `File:ColorComponents`.

Where a photo was taken and whose camera took it stay private: GPS and
other location fields, serial numbers and owners' names are never read
into it. The original file itself is kept as it was uploaded, so
**Download original** still has whatever it contained.

Posts uploaded before metadata was read get it when their files are
processed again: `moekura admin regenerate-media --all`.
