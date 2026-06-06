#!/usr/bin/env bash
# 给「心上」后端填 LOVEAI 的 API key。本文件【不含任何密钥】。
# 自动从 桌面/API.rtf 读 key（精确匹配 sk- 开头的 51 位左右），读不到再提示粘贴。
# 全程在你本机运行，key 不经过 Claude。
cd "/Users/fiveowu/Documents/Claude/Projects/心上/backend" || { echo "❌ 找不到后端目录"; exit 1; }

K=""
for f in "$HOME/Desktop/API.rtf" "$HOME/Desktop/API.txt" "$HOME/Desktop/api.rtf" "$HOME/Desktop/api.txt"; do
  if [ -f "$f" ]; then
    K=$(grep -oE 'sk-[A-Za-z0-9]{40,60}' "$f" | head -1)
    if [ -n "$K" ]; then echo "  ✓ 从 $(basename "$f") 读到了 key"; break; fi
  fi
done

if [ -z "$K" ]; then
  echo ""
  echo "  没自动读到。请把 Moonshot key 粘进来，按回车："
  read -r K
  K="$(printf '%s' "$K" | tr -d '[:space:]')"
fi

if [ "${#K}" -lt 30 ]; then
  echo "  ❌ key 太短(${#K}位)，可能没读对，文件没改动。"
  exit 1
fi

if grep -q '^AI_API_KEY_BACKUP=' .env 2>/dev/null; then
  sed -i.bak "s|^AI_API_KEY_BACKUP=.*|AI_API_KEY_BACKUP=$K|" .env && rm -f .env.bak
else
  printf '\nAI_API_KEY_BACKUP=%s\nAI_BASE_URL_BACKUP=https://api.moonshot.cn/v1\nAI_MODEL_BACKUP=moonshot-v1-8k\n' "$K" >> .env
fi

echo ""
echo "  ✅ 已写入 backend/.env，key 长度 = ${#K}"
echo "  回去跟 Claude 说「好了」，它会重启后端、截真实对话。"
echo ""
