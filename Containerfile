# Same glibc aarch64 Bun build that runs on the Pi's 16 KB-page kernel.
ARG BUN_IMAGE=docker.io/oven/bun:1.4.3-slim@sha256:14a223fa35747ec728bce300ef85888bc5f7958164d15bbba6a54b22b52a3288

FROM ${BUN_IMAGE} AS build
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile
COPY . .
RUN bun run build

FROM ${BUN_IMAGE}
WORKDIR /app
RUN groupadd --gid 10001 spareline \
	&& useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin spareline
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production && rm -rf /root/.bun/install/cache
COPY src ./src
COPY vendor ./vendor
COPY --from=build /app/dist ./dist
RUN mkdir -p /app/data && chown 10001:10001 /app/data

ARG REVISION=dev
ENV NODE_ENV=production \
	HOST=0.0.0.0 \
	PORT=8740 \
	DATA_DIR=/app/data \
	WEB_DIR=/app/dist/web \
	SPARELINE_REVISION=${REVISION} \
	BUN_RUNTIME_TRANSPILER_CACHE_PATH=0
LABEL org.opencontainers.image.source="https://github.com/265866/spareline" \
	org.opencontainers.image.revision="${REVISION}"

USER 10001:10001
VOLUME ["/app/data"]
EXPOSE 8740
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
	CMD ["bun", "-e", "const r = await fetch('http://127.0.0.1:' + (process.env.PORT ?? '8740') + '/healthz'); if (!r.ok) process.exit(1)"]
CMD ["bun", "src/server.ts"]
