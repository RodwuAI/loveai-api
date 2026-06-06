#!/usr/bin/env bash
# 把 AI_MODEL 改成你的火山方舟接入点ID(ep-)或正确模型名，重启容器并自测。
# 用法： bash fix-model.sh ep-2024xxxx-xxxxx      或      bash fix-model.sh （交互输入）
set -e
M="$1"
if [ -z "$M" ]; then read -p "粘贴火山方舟接入点ID(ep-开头)或正确模型名: " M; fi
[ -z "$M" ] && { echo "❌ 没输入，退出。"; exit 1; }

sed -i "s|^AI_MODEL=.*|AI_MODEL=$M|" .env
echo ">> 已设 AI_MODEL=$M，重启容器..."
sudo docker rm -f loveai 2>/dev/null || true
sudo docker run -d --restart always -p 80:8800 --env-file .env --name loveai loveai-api >/dev/null
sleep 3

echo ">> 自测真实 AI（豆包）："
curl -s -X POST http://localhost/ai/qa \
  -H 'content-type: application/json' -H 'x-user-id: test' \
  -d '{"person_name":"妈妈","relation":"母亲","memories":[{"content":"妈妈最近爱跳广场舞","category":"日常","created_at":"2026-05-01T08:00:00Z"}],"question":"这周末想陪妈妈，聊点什么好？","history":[]}'
echo; echo
echo "看上面：出现一段关心妈妈的回答 = 豆包通了 ✅ ；还是 error = 把这段发我。"
