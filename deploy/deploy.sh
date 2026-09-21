#!/usr/bin/env bash
# ThirdC 部署：rsync 源码到服务器，服务器端构建（首次较慢），systemd 托管
# 用法：./deploy/deploy.sh [--skip-build]
set -euo pipefail
SSH_KEY="$(dirname "$0")/../.deploy-ssh/key"
SSH_PORT="28766"; USER_="root"; HOST="172.96.253.73"
REMOTE_DIR="/www/wwwroot/thirdc"
SSH_CMD="ssh -i ${SSH_KEY} -p ${SSH_PORT} -o StrictHostKeyChecking=no ${USER_}@${HOST}"
SKIP_BUILD=${1:-}
rsync -az --delete -e "ssh -i ${SSH_KEY} -p ${SSH_PORT} -o StrictHostKeyChecking=no" \
  --exclude '.git' --exclude 'target' --exclude '.thirdc' \
  --exclude 'server-kb' --exclude '.deploy-ssh' \
  --exclude '.v2c' --exclude '.video_agent' --exclude '.DS_Store' \
  ./ "${USER_}@${HOST}:${REMOTE_DIR}/src/"
$SSH_CMD "cd ${REMOTE_DIR}/src && ${SKIP_BUILD:+echo skip-build || }cargo build --release -p thirdc 2>&1 | tail -1; mkdir -p bin kb && cp target/release/thirdc bin/ && true"
$SSH_CMD "cp ${REMOTE_DIR}/src/deploy/thirdc.service /etc/systemd/system/ && systemctl daemon-reload && systemctl enable --now thirdc"
echo "ThirdC: https://nownexts.com/thirdc/  (daemon on 127.0.0.1:7700)"
