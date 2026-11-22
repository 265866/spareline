# The build stage reads rust-toolchain.toml, so the image builds with the same Rust as CI. trellis's
# Renovate moves the pinned digests, and they arrive with trellis updates. `just notices` writes
# the license notices under target/ first; `just image` and the workflows run it.
#
# The build stages run on the builder's own platform and cross-compile for the image's, so one
# runner builds every architecture without emulating one; the final stage only copies files.
FROM --platform=$BUILDPLATFORM docker.io/library/rust:slim-trixie@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe AS build
ARG BUILDARCH
ARG TARGETARCH
WORKDIR /src
# Another architecture needs Debian's cross gcc, which links the program and which cc finds on its
# own to build the C code some crates have.
RUN if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      case "$TARGETARCH" in \
        amd64) gcc=gcc-x86-64-linux-gnu ;; \
        arm64) gcc=gcc-aarch64-linux-gnu ;; \
        *) echo "error: no cross gcc for $TARGETARCH" >&2; exit 1 ;; \
      esac; \
      apt-get update && apt-get install --yes --no-install-recommends "$gcc" "libc6-dev-$TARGETARCH-cross" \
        && rm -rf /var/lib/apt/lists/*; \
    fi
COPY . .
# The image's files are data, so the program the image runs stays the checked binary: none may be
# a link or an ELF file, or land where the base image keeps its own files and directories, which
# the copy would replace or give root's owner and mode, such as /usr/lib or /tmp. /usr/share and a
# directory of the project's own, such as /app, are open to them.
RUN files=crates/spareline/image; \
    if [ ! -d "$files" ]; then \
      echo "error: the image answer sets files: true, but $files doesn't exist" >&2; exit 1; \
    fi; \
    for own in bin boot dev etc home lib lib32 lib64 libx32 proc root run sbin sys tmp var; do \
      if [ -e "$files/$own" ] || [ -L "$files/$own" ]; then \
        echo "error: $files/$own would change the image's own /$own; use /usr/share or a directory of its own, such as /app" >&2; exit 1; \
      fi; \
    done; \
    for entry in "$files"/usr/* "$files"/usr/.[!.]*; do \
      if { [ -e "$entry" ] || [ -L "$entry" ]; } && [ "${entry#"$files"/}" != usr/share ]; then \
        echo "error: ${entry#"$files"/} would change the image's own /${entry#"$files"/}; only /usr/share takes the image's files" >&2; exit 1; \
      fi; \
    done; \
    link=$(find "$files" -type l -print -quit); \
    if [ -n "$link" ]; then echo "error: $link is a link; the image's files must be files" >&2; exit 1; fi; \
    find "$files" -type f -exec sh -c 'head -c 4 "$1" | grep -q "^.ELF"' sh {} \; -print > /tmp/elf; \
    if [ -s /tmp/elf ]; then echo "error: $(head -n 1 /tmp/elf) is an ELF program or library; the image runs only the checked binary" >&2; exit 1; fi
RUN case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "error: no Rust target for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    target="$arch-unknown-linux-gnu"; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      export "CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_LINKER=$arch-linux-gnu-gcc"; \
    fi; \
    rustup toolchain install --no-self-update && rustup target add "$target" \
      && cargo build --locked --release --package spareline --target "$target" \
      && cp "target/$target/release/spareline" /spareline
# The image runs as a non-root user, so its license files must be readable by all, whatever modes
# the checkout and `just notices` gave them. COPY --chmod would set the directory BuildKit creates
# to the same mode, which the user can't enter.
RUN install -D --mode=644 --target-directory=/licenses target/THIRD-PARTY-LICENSES.html target/THIRD-PARTY-LICENSES-WEB.txt LICENSE-MIT LICENSE-APACHE
# What the image adds, from the image answer, gets the same treatment: readable by all and
# writable by none but root, the working directory included. No file is executable, so all that
# runs is the checked binary.
RUN mkdir -p /files/app && cp -R crates/spareline/image/. /files/ \
      && find /files -type f -exec chmod a-x {} + && chmod -R u+rwX,go+rX,go-w /files

# The web UI builds with the Bun version the workflows and CONTRIBUTING.md pin, from its lockfile.
# Its whole directory goes in before the install, so the install reads the same bunfig.toml and
# .npmrc as `just web-install`. As there, it fails if bun.lock is missing, since the install would
# then pick the versions itself, and if a package provides a `bun` or `bunx` command, which
# `bun run build` would run instead of this image's Bun.
FROM --platform=$BUILDPLATFORM docker.io/oven/bun:1.4.4-slim@sha256:1846adf22496bd711036aae8be147e57bdebbaea2e5264e3e0b5af1db187d9a4 AS web
WORKDIR /web
COPY crates/spareline/web/ ./
RUN if [ ! -f bun.lock ]; then \
      echo "error: the web UI has no bun.lock to install from" >&2; exit 1; \
    fi \
      && bun install --frozen-lockfile \
      && if [ -e node_modules/.bin/bun ] || [ -e node_modules/.bin/bunx ]; then \
           echo "error: node_modules/.bin holds a bun or bunx command, so the build would run on that Bun and not this image's; see CONTRIBUTING.md" >&2; exit 1; \
         fi
RUN bun run build

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cb87acc574abe719b6e5c547384defa2f7b1f24af9605d3f618cf852495d8be2
LABEL org.opencontainers.image.source="https://github.com/265866/spareline" \
      org.opencontainers.image.description="Spare phone lines on spare phones, with an inbox for each line." \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
# The image's own files go first, so the binary and the license files below always win.
COPY --from=build /files/ /
COPY --from=build /licenses/ /usr/share/doc/spareline/
COPY --from=build /spareline /usr/local/bin/spareline
# The service reads SPARELINE_WEB_DIR to find the web UI it serves.
COPY --from=web /web/dist /usr/share/spareline/web
ENV SPARELINE_WEB_DIR=/usr/share/spareline/web
ENV DATA_DIR="/app/data" \
    HOST="0.0.0.0" \
    PORT="8740"
WORKDIR /app
EXPOSE 8740
ENTRYPOINT ["/usr/local/bin/spareline"]
