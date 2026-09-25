# syntax=docker/dockerfile:1

FROM docker.io/library/rust:1.98-trixie AS chef
ARG CARGO_CHEF_VERSION=0.1.78
RUN cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"
WORKDIR /src

# The dependency list, from the manifests and Cargo.lock only.
FROM chef AS plan
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Dependencies build in a layer of their own, which layer caches (CI's
# and release's) keep until a manifest or Cargo.lock changes; then only
# Moekura's crates build. No cache mounts: what's in them never reaches
# the layers.
FROM chef AS build
COPY --from=plan /src/recipe.json recipe.json
RUN cargo chef cook --release --locked -p moekura --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked -p moekura \
    && cp target/release/moekura /usr/local/bin/moekura

# ffmpeg and libvips, built with only what Moekura uses. Debian's packages
# pull in hundreds of megabytes it never touches (Mesa and LLVM through
# ffmpeg's device support; ImageMagick, Poppler, HDF5, OpenEXR and more
# through libvips).
FROM docker.io/library/debian:trixie-slim AS media
ARG FFMPEG_VERSION=9.0.2
ARG FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
ARG HEIF_VERSION=1.23.5
ARG HEIF_SHA256=fd9036064c4432f0550d15072ddf34956a248279ee9aeaff0fba3fa0f77d8f1a
ARG VIPS_VERSION=8.18.6
ARG VIPS_SHA256=3c41e1d5458081bfa4a5bc54e116c46259c75c6760a18027764555632b9dda3e
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils build-essential pkg-config nasm meson ninja-build cmake \
        zlib1g-dev libdav1d-dev libaom-dev \
        libglib2.0-dev libexpat1-dev libjpeg62-turbo-dev libpng-dev libwebp-dev \
        libjxl-dev libexif-dev liblcms2-dev libhwy-dev
WORKDIR /build
# ffmpeg: reading MP4 and WebM (H.264, VP8, VP9, AV1) for probing, and
# writing a PNG poster frame.
RUN curl -fsSLo ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz" \
    && echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c - \
    && tar xJf ffmpeg.tar.xz \
    && cd "ffmpeg-${FFMPEG_VERSION}" \
    && ./configure --prefix=/opt/media \
        --disable-everything --disable-autodetect --disable-doc --disable-debug \
        --disable-network --disable-ffplay \
        --enable-zlib --enable-libdav1d \
        --enable-protocol=file,pipe \
        --enable-demuxer=mov,matroska \
        --enable-parser=h264,vp8,vp9,av1 \
        --enable-decoder=h264,vp8,vp9,libdav1d \
        --enable-encoder=png --enable-muxer=image2 \
        --enable-filter=scale \
    && make -j"$(nproc)" \
    && make install
# libheif for AVIF, with dav1d (decoding) and aom (encoding) built in.
# libvips only trusts libheif 1.23.2 and later with hostile files.
RUN curl -fsSLo heif.tar.gz "https://github.com/strukturag/libheif/releases/download/v${HEIF_VERSION}/libheif-${HEIF_VERSION}.tar.gz" \
    && echo "${HEIF_SHA256}  heif.tar.gz" | sha256sum -c - \
    && tar xzf heif.tar.gz \
    && cmake -S "libheif-${HEIF_VERSION}" -B heif-build -G Ninja \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/opt/media -DCMAKE_INSTALL_LIBDIR=lib \
        -DENABLE_PLUGIN_LOADING=OFF \
        -DWITH_DAV1D=ON -DWITH_DAV1D_PLUGIN=OFF \
        -DWITH_AOM_ENCODER=ON -DWITH_AOM_ENCODER_PLUGIN=OFF -DWITH_AOM_DECODER=OFF \
        -DWITH_LIBDE265=OFF -DWITH_X265=OFF -DWITH_X264=OFF -DWITH_OpenH264_DECODER=OFF \
        -DWITH_EXAMPLES=OFF -DWITH_GDK_PIXBUF=OFF -DBUILD_TESTING=OFF -DBUILD_DOCUMENTATION=OFF \
    && ninja -C heif-build \
    && ninja -C heif-build install
# libvips: JPEG, PNG, GIF, WebP, HEIF/AVIF and JPEG XL, with EXIF
# orientation and colour management, plus PGM for perceptual hashes.
RUN curl -fsSLo vips.tar.xz "https://github.com/libvips/libvips/releases/download/v${VIPS_VERSION}/vips-${VIPS_VERSION}.tar.xz" \
    && echo "${VIPS_SHA256}  vips.tar.xz" | sha256sum -c - \
    && tar xJf vips.tar.xz \
    && PKG_CONFIG_PATH=/opt/media/lib/pkgconfig meson setup vips-build "vips-${VIPS_VERSION}" \
        --prefix=/opt/media --libdir=lib --buildtype=release \
        --auto-features=disabled \
        -Ddeprecated=false -Dexamples=false -Dcplusplus=false -Dmodules=disabled \
        -Djpeg=enabled -Dpng=enabled -Dwebp=enabled -Dheif=enabled -Djpeg-xl=enabled \
        -Dexif=enabled -Dlcms=enabled -Dhighway=enabled -Dzlib=enabled \
        -Dnsgif=true -Dppm=true -Danalyze=false -Dradiance=false \
    && ninja -C vips-build \
    && ninja -C vips-build install \
    && rm -rf /opt/media/include /opt/media/share /opt/media/lib/pkgconfig \
        /opt/media/bin/vipsprofile /opt/media/bin/vips-*

FROM docker.io/library/debian:trixie-slim
# The libraries the media tools above link to.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libdav1d7 libaom3 \
        libglib2.0-0t64 libexpat1 libjpeg62-turbo libpng16-16t64 \
        libwebp7 libwebpmux3 libwebpdemux2 libsharpyuv0 \
        libjxl0.11 libexif12 liblcms2-2 libhwy1t64 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/moekura --create-home moekura \
    && install -d -o moekura -g moekura /var/lib/moekura/data
COPY --from=media /opt/media /usr/local
# Fails the build if a library is missing.
RUN ldconfig \
    && ! ldd /usr/local/bin/* /usr/local/lib/*.so | grep "not found"
COPY --from=build /usr/local/bin/moekura /usr/local/bin/moekura
USER moekura
WORKDIR /var/lib/moekura
# Stored files (storage.path defaults to ./data); mount a volume here.
VOLUME /var/lib/moekura/data
EXPOSE 8080
ENTRYPOINT ["moekura"]
CMD ["serve"]
