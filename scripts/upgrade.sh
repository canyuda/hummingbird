#!/usr/bin/env bash
# 蜂鸟（Hummingbird）升级脚本：从 GitHub Releases 下载对应架构的二进制，校验后替换并重启服务。
#
# 用法：
#   sudo bash upgrade.sh                 # 升级到最新版
#   sudo bash upgrade.sh v0.1.1          # 升级到指定版本
#
# 环境变量：
#   HB_INSTALL_DIR   安装目录（默认 /opt/hummingbird）
#   HB_GH_PROXY      GitHub 下载代理前缀（如 https://ghproxy.net/，国内网络加速）
#                    例：curl -fsSL .../upgrade.sh | sudo env HB_GH_PROXY=https://ghproxy.net/ bash
# 依赖：curl、tar、sha256sum；仅替换二进制，数据（hummingbird.db）不动。
set -euo pipefail

REPO="canyuda/hummingbird"
INSTALL_DIR="${HB_INSTALL_DIR:-/opt/hummingbird}"
GH="${HB_GH_PROXY}https://github.com"

[ "$(id -u)" -eq 0 ] || { echo "错误：需要 root 权限（sudo bash upgrade.sh）"; exit 1; }

# 架构 → Release 产物平台名（须与 .github/workflows/build.yml 的 platform 保持一致）
case "$(uname -m)" in
  aarch64)      PLATFORM="linux-arm64" ;;
  armv7l)       PLATFORM="linux-armv7" ;;
  x86_64)       PLATFORM="linux-amd64" ;;
  armv6l)       echo "错误：Pi Zero/1（armv6）不在发布目标内"; exit 1 ;;
  *)            echo "错误：不支持的架构 $(uname -m)"; exit 1 ;;
esac

# 版本：参数指定，或跟随 releases/latest 重定向解析
if [ -n "${1:-}" ]; then
  VERSION="$1"
else
  VERSION=$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$GH/$REPO/releases/latest" | sed 's#.*/tag/##')
fi
case "$VERSION" in
  v*) ;;
  *) echo "错误：无法解析最新版本号（$VERSION），请显式指定：sudo bash upgrade.sh v0.1.1"; exit 1 ;;
esac

ASSET="hummingbird-$VERSION-$PLATFORM.tar.gz"
URL_BASE="$GH/$REPO/releases/download/$VERSION"

TMP=$(mktemp -d /tmp/hb-upgrade.XXXXXX)
trap 'rm -rf "$TMP"' EXIT

echo "==> 下载 $URL_BASE/$ASSET"
curl -fSL --retry 3 -o "$TMP/$ASSET" "$URL_BASE/$ASSET"

# 校验文件是 v0.1.2 起随 Release 提供；旧版本缺失时警告跳过
if curl -fSL --retry 3 -o "$TMP/$ASSET.sha256" "$URL_BASE/$ASSET.sha256" 2>/dev/null; then
  echo "==> 校验 sha256"
  (cd "$TMP" && sha256sum -c "$ASSET.sha256")
else
  echo "==> 警告：该版本未提供 sha256 校验文件，跳过校验" >&2
fi

echo "==> 解压"
tar -xzf "$TMP/$ASSET" -C "$TMP"

# systemd 服务存在则先停（可能未在运行，容忍失败）
SYSTEMD=0
if systemctl cat hummingbird.service >/dev/null 2>&1; then SYSTEMD=1; fi
[ "$SYSTEMD" -eq 1 ] && { systemctl stop hummingbird || true; }

echo "==> 替换 $INSTALL_DIR/hummingbird"
mkdir -p "$INSTALL_DIR"
[ -f "$INSTALL_DIR/hummingbird" ] && cp -p "$INSTALL_DIR/hummingbird" "$INSTALL_DIR/hummingbird.bak"
install -m 0755 "$TMP/hummingbird" "$INSTALL_DIR/hummingbird"

if [ "$SYSTEMD" -eq 1 ]; then
  systemctl start hummingbird
  sleep 1
  systemctl --no-pager --lines=3 status hummingbird >/dev/null 2>&1 || true
  echo "==> 完成：已升级到 $VERSION（旧版备份为 hummingbird.bak，服务已重启）"
else
  echo "==> 完成：已升级到 $VERSION（旧版备份为 hummingbird.bak）；未检测到 systemd 服务，请手动重启进程"
fi
