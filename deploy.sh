#!/usr/bin/env bash
# 在腾讯云服务器上一键部署 LOVEAI 后端。
# 前置：本目录有 Dockerfile/src/Cargo.toml，且你已创建好填了 key 的 .env。
set -e

# 1. 装 Docker（已装则跳过）
if ! command -v docker >/dev/null 2>&1; then
  echo ">> 安装 Docker..."
  curl -fsSL https://get.docker.com | sh
fi

# 2. 检查 .env
if [ ! -f .env ]; then
  echo "❌ 缺 .env。先创建并填入你的 key（见 .env.example），再重跑。"
  exit 1
fi

# 3. 构建 + 运行（80 端口对外，容器内 8800；崩溃自动重启）
echo ">> 构建镜像..."
docker build -t loveai-api .
# 账号/用量/会员 JSON 存到容器外宿主机目录，重建容器不丢数据。
mkdir -p "$HOME/loveai-data"
docker rm -f loveai 2>/dev/null || true
docker run -d --restart always -p 80:8800 --env-file .env \
  -v "$HOME/loveai-data":/app --name loveai loveai-api

# 4. 自检
sleep 3
echo ">> 健康检查："
curl -fsS http://localhost/health && echo "  ✅ 后端已起" || echo "  ⚠️ 没起来，看 docker logs loveai"
