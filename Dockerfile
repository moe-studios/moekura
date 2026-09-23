# syntax=docker/dockerfile:1

FROM docker.io/library/rust:1.98-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p uwuubooru \
    && cp target/release/uwuubooru /usr/local/bin/uwuubooru

FROM docker.io/library/debian:trixie-slim
# libvips for images (AVIF via the libheif plugins, JPEG XL built in) and
# ffmpeg for video. Together they are most of the image's size.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libvips-tools \
        libheif-plugin-dav1d \
        libheif-plugin-aomenc \
        ffmpeg \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/uwuubooru --create-home uwuubooru \
    && install -d -o uwuubooru -g uwuubooru /var/lib/uwuubooru/data
COPY --from=build /usr/local/bin/uwuubooru /usr/local/bin/uwuubooru
USER uwuubooru
WORKDIR /var/lib/uwuubooru
# Stored files (storage.path defaults to ./data); mount a volume here.
VOLUME /var/lib/uwuubooru/data
EXPOSE 8080
ENTRYPOINT ["uwuubooru"]
CMD ["serve"]
