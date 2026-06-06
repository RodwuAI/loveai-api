#!/usr/bin/env bash
# 交互式生成 .env —— 脚本里没有任何明文密钥。
# 密钥由你运行时输入：不回显、不上传、只写到本机 .env（权限 600）。
set -e

echo "== LOVEAI 后端 .env 生成（主力：豆包 / 火山方舟）=="
echo "粘贴 key 时屏幕不显示，是正常的。"
echo

read -s -p "粘贴豆包 API key: " DOUBAO; echo
if [ -z "$DOUBAO" ]; then echo "❌ 没输入 key，已退出。"; exit 1; fi
read -p "豆包接入点ID或模型名 [默认 doubao-pro-32k]: " DM

{
  echo "AI_BASE_URL=https://ark.cn-beijing.volces.com/api/v3"
  echo "AI_MODEL=${DM:-doubao-pro-32k}"
  echo "AI_API_KEY=$DOUBAO"
  echo "AI_FREE_LIMIT=10"
  echo "PORT=8800"
} > .env

# 可选：Moonshot 作备用容灾
read -p "加 Moonshot 做备用容灾? [y/N]: " ADD
if [ "$ADD" = "y" ] || [ "$ADD" = "Y" ]; then
  read -s -p "粘贴 Moonshot key (sk-...): " MOON; echo
  {
    echo "AI_API_KEY_BACKUP=$MOON"
    echo "AI_BASE_URL_BACKUP=https://api.moonshot.cn/v1"
    echo "AI_MODEL_BACKUP=moonshot-v1-8k"
  } >> .env
fi

# 可选：生图模型
read -p "有生图模型名就填，没有直接回车: " IMG
[ -n "$IMG" ] && echo "AI_IMAGE_MODEL=$IMG" >> .env

chmod 600 .env
echo
echo "✅ .env 已生成（权限 600，主力豆包）。下一步：bash deploy.sh"
