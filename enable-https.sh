#!/usr/bin/env bash
# 在 deploy.sh 跑通、且 DNS（api.loveaivip.com → 本机IP）生效后运行。
# 作用：把 API 收回内网，由 Caddy 占 80/443 自动签 HTTPS 证书并反代。
set -e
DOMAIN="${DOMAIN:-api.loveaivip.com}"

if [ ! -f .env ]; then echo "❌ 缺 .env，先 bash setup-env.sh"; exit 1; fi

echo ">> API 改为只绑本机 8800（把公网 80 让给 Caddy）..."
docker rm -f loveai 2>/dev/null || true
docker run -d --restart always -p 127.0.0.1:8800:8800 --env-file .env --name loveai loveai-api

echo ">> 启动 Caddy 自动 HTTPS（$DOMAIN → 本机 8800）..."
docker rm -f caddy 2>/dev/null || true
docker run -d --restart always --network host -v caddy_data:/data --name caddy caddy \
  caddy reverse-proxy --from "$DOMAIN" --to 127.0.0.1:8800

sleep 5
echo
echo "✅ HTTPS 已启。证书自动签发约几十秒。"
echo "   测试：浏览器开 https://$DOMAIN/health"
echo "   看进度/排错：docker logs caddy"
