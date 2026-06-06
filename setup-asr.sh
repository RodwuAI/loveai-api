#!/usr/bin/env bash
# 给 .env 追加火山 ASR 配置。token 运行时输入：不回显、不写进脚本。
set -e
[ -f .env ] || { echo "❌ 缺 .env，先 bash setup-env.sh"; exit 1; }
sed -i '/^VOLC_ASR_/d' .env 2>/dev/null || true   # 清旧的，避免重复

read -p "火山 ASR App ID [默认 5414024970]: " AID
read -s -p "火山 ASR Access Key（第2个 key，粘贴）: " TOK; echo
[ -z "$TOK" ] && { echo "❌ 没输入 token，已退出。"; exit 1; }

{
  echo "VOLC_ASR_APP_ID=${AID:-5414024970}"
  echo "VOLC_ASR_TOKEN=$TOK"
  echo "VOLC_ASR_RESOURCE_ID=volc.bigasr.sauc.duration"
} >> .env
echo "✅ 火山 ASR 配置已写入 .env。下一步：bash redeploy.sh"
