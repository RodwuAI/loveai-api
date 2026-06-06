# 多阶段构建：编译用全工具链，运行用精简镜像
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
# reqwest(rustls) 调 https 厂商需要根证书
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/xinshang_backend /usr/local/bin/xinshang_backend
# 多数平台会注入 PORT；main.rs 读 PORT，默认 8800
ENV PORT=8800
EXPOSE 8800
# AI key 等机密由平台的环境变量/密钥管理注入，绝不打进镜像
CMD ["xinshang_backend"]
