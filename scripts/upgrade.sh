#!/usr/bin/env bash
# 蜂鸟（Hummingbird）升级脚本：从 GitHub Releases 下载对应架构的二进制，校验后替换并重启服务。
#
# 用法：
#   sudo bash upgrade.sh                 # 升级到最新版
#   sudo bash upgrade.sh v0.1.1          # 升级到指定版本
#
# 环境变量：
#   HB_INSTALL_DIR          安装目录（默认 /opt/hummingbird）
#   HB_GH_PROXY             GitHub 下载代理前缀（如 https://ghproxy.net/，国内网络加速）
#                           例：curl -fsSL .../upgrade.sh | sudo env HB_GH_PROXY=https://ghproxy.net/ bash
#   HB_ALLOW_NO_CHECKSUM=1  版本无 sha256 校验文件时仍继续（默认拒绝替换，防不可信来源）
# 依赖：curl、tar、sha256sum；仅替换二进制，数据（hummingbird.db）不动。
set -euo pipefail

REPO="canyuda/hummingbird"
INSTALL_DIR="${HB_INSTALL_DIR:-/opt/hummingbird}"
GH="${HB_GH_PROXY:-}https://github.com"

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

# 校验文件随 Release 提供；缺失默认拒绝替换（二进制不动），确需跳过时显式豁免
if curl -fSL --retry 3 -o "$TMP/$ASSET.sha256" "$URL_BASE/$ASSET.sha256" 2>/dev/null; then
  echo "==> 校验 sha256"
  (cd "$TMP" && sha256sum -c "$ASSET.sha256")
elif [ "${HB_ALLOW_NO_CHECKSUM:-0}" = "1" ]; then
  echo "==> 警告：HB_ALLOW_NO_CHECKSUM=1，已跳过 sha256 校验" >&2
else
  echo "错误：$VERSION 未提供 sha256 校验文件，已中止（二进制未动）。确需跳过：sudo env HB_ALLOW_NO_CHECKSUM=1 bash upgrade.sh" >&2
  exit 1
fi

echo "==> 解压"
tar -xzf "$TMP/$ASSET" -C "$TMP"

# systemd 服务存在则先停（可能未在运行，容忍失败）
SYSTEMD=0
if systemctl cat hummingbird.service >/dev/null 2>&1; then SYSTEMD=1; fi
if [ "$SYSTEMD" -eq 1 ]; then systemctl stop hummingbird || true; fi

echo "==> 替换 $INSTALL_DIR/hummingbird"
mkdir -p "$INSTALL_DIR"
if [ -f "$INSTALL_DIR/hummingbird" ]; then
  cp -p "$INSTALL_DIR/hummingbird" "$INSTALL_DIR/hummingbird.bak"
fi
install -m 0755 "$TMP/hummingbird" "$INSTALL_DIR/hummingbird"

if [ "$SYSTEMD" -eq 1 ]; then
  systemctl start hummingbird
  sleep 1
  systemctl --no-pager --lines=3 status hummingbird >/dev/null 2>&1 || true
  echo "==> 完成：已升级到 $VERSION（旧版备份为 hummingbird.bak，服务已重启）"
else
  echo "==> 完成：已升级到 $VERSION（旧版备份为 hummingbird.bak）；未检测到 systemd 服务，请手动重启进程"
fi
