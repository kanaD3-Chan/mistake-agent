# syntax=docker/dockerfile:1

# ── 构建阶段 ────────────────────────────────────────────────────────────────
# 基础镜像可用 `--build-arg RUST_IMAGE=...` 覆盖：网络受限（镜像仓库不可达）时
# 用本机已有的等价镜像离线构建，不必改这个文件。
ARG RUST_IMAGE=rust:1-bookworm
FROM ${RUST_IMAGE} AS builder
WORKDIR /build

# native-tls 在 Linux 上走 OpenSSL（我们在 Cargo.toml 里刻意避开带 C 代码的 ring/aws-lc，
# 见文件内注释）。构建期需要头文件与 pkg-config。
RUN apt-get update \
 && apt-get install -y --no-install-recommends libssl-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*

# 先只拷 manifest，用一个空壳 crate 把**依赖层**单独缓存下来。
# 注意：`sqlx::migrate!` 要求编译期存在 migrations/，所以空壳阶段不能拷真实 src/。
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
 && echo 'fn main() {}' > src/main.rs \
 && echo '' > src/lib.rs \
 && cargo build --release --locked \
 && rm -rf src

# 再拷真实源码与迁移（迁移由 sqlx::migrate! 编译期嵌入，运行期不需要这个目录）
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# ── 运行阶段 ────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# ca-certificates：访问 DeepSeek 的 HTTPS 需要根证书
# libssl3：native-tls 的运行期依赖
# curl：仅用于下面的 HEALTHCHECK
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --shell /usr/sbin/nologin app

COPY --from=builder /build/target/release/mistake-agent-server /usr/local/bin/mistake-agent-server

USER app

# 容器内必须监听 0.0.0.0 才能被同网络的 Caddy 访问。
# ⚠️ **不要把 8080 发布到宿主机/公网**：这个端口没有 TLS，也没有 Caddy 那一层。
ENV BIND_ADDR=0.0.0.0:8080 \
    RUST_LOG=info
EXPOSE 8080

# 存活探针：只探进程，不碰数据库（与 /readyz 的分工一致）
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/mistake-agent-server"]
