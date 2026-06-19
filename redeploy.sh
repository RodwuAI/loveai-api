#!/usr/bin/env bash
# 拉最新代码 + 重建镜像 + 重启 API（Caddy 在前，API 只绑本机 8800）。
# 用法：bash redeploy.sh   （docker 用 sudo，git pull 用当前用户）
set -e
echo ">> 拉最新代码..."
git pull --ff-only
echo ">> 重建镜像（Rust 编译，几分钟）..."
sudo docker build -t loveai-api .
echo ">> 重启 API 容器..."
# 账号/用量/会员 JSON 存到容器外宿主机目录，redeploy 重建容器不丢数据。
mkdir -p "$HOME/loveai-data"
sudo docker rm -f loveai 2>/dev/null || true
sudo docker run -d --restart always -p 127.0.0.1:8800:8800 --env-file .env \
  -v "$HOME/loveai-data":/app --name loveai loveai-api >/dev/null
sleep 3
curl -fsS http://127.0.0.1:8800/health && echo "  ✅ 重新部署完成（API 已更新）" || echo "  ⚠️ 没起来，看 sudo docker logs loveai"
