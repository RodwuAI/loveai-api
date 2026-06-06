#!/usr/bin/env bash
# 自动找出你火山账号能用的豆包模型ID，写入 .env，重启容器并自测。
# 不传参=自动逐个探测；也可手动指定： bash fix-model.sh doubao-pro-32k-241215  或  bash fix-model.sh ep-xxxx
set -e

KEY=$(grep -E '^AI_API_KEY=' .env | head -1 | cut -d= -f2-)
BASE=$(grep -E '^AI_BASE_URL=' .env | head -1 | cut -d= -f2-)
[ -z "$KEY" ] && { echo "❌ .env 里没有 AI_API_KEY，先跑 bash setup-env.sh"; exit 1; }
[ -z "$BASE" ] && BASE="https://ark.cn-beijing.volces.com/api/v3"

if [ -n "$1" ]; then
  CANDS=("$@")
else
  CANDS=(
    doubao-pro-32k-241215
    doubao-1-5-pro-32k-250115
    doubao-pro-32k-240828
    doubao-1-5-pro-32k-character-250228
    doubao-pro-32k-character-241215
    doubao-seed-1-6-250615
    doubao-pro-4k-240515
    doubao-lite-32k-240828
  )
fi

FOUND=""
echo ">> 用你的 key 直连火山，逐个试模型，找能用的（key 不外传）..."
for M in "${CANDS[@]}"; do
  printf "   %-36s " "$M"
  R=$(curl -s -m 20 -X POST "$BASE/chat/completions" \
        -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
        -d "{\"model\":\"$M\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":1}")
  if echo "$R" | grep -q '"choices"'; then
    echo "✅ 能用"; FOUND="$M"; break
  elif echo "$R" | grep -qi 'NotFound\|does not exist\|InvalidEndpointOrModel'; then
    echo "✗ 未开通"
  else
    echo "✗ $(echo "$R" | tr -d '\n' | head -c 70)"
  fi
done

if [ -z "$FOUND" ]; then
  echo
  echo "❌ 这些都没开通。两条路："
  echo "   1) 去火山「开通管理」开通一个豆包模型，再重跑本脚本；"
  echo "   2) 把你能跑的那个项目用的模型ID发我： bash fix-model.sh 那个ID"
  exit 1
fi

echo ">> 命中：$FOUND，写入 .env 并重启容器..."
sed -i "s|^AI_MODEL=.*|AI_MODEL=$FOUND|" .env
sudo docker rm -f loveai 2>/dev/null || true
sudo docker run -d --restart always -p 80:8800 --env-file .env --name loveai loveai-api >/dev/null
sleep 3
echo ">> 自测真实 LOVEAI："
curl -s -X POST http://localhost/ai/qa -H 'content-type: application/json' -H 'x-user-id: test' \
  -d '{"person_name":"妈妈","relation":"母亲","memories":[{"content":"妈妈最近爱跳广场舞","category":"日常","created_at":"2026-05-01T08:00:00Z"}],"question":"这周末想陪妈妈，聊点什么好？","history":[]}'
echo; echo
echo "✅ 上面出现一段关心妈妈的回答 = 全通了！模型已锁定 $FOUND"
