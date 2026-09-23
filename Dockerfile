# syntax=docker/dockerfile:1

FROM docker.io/library/rust:1.98-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p uwuubooru \
    && cp target/release/uwuubooru /usr/local/bin/uwuubooru

FROM docker.io/library/debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /var/lib/uwuubooru --create-home uwuubooru
COPY --from=build /usr/local/bin/uwuubooru /usr/local/bin/uwuubooru
USER uwuubooru
WORKDIR /var/lib/uwuubooru
EXPOSE 8080
ENTRYPOINT ["uwuubooru"]
CMD ["serve"]
