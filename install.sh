#!/usr/bin/env sh
# matrix-satori 一键安装脚本
# 从 GitHub Releases 下载预编译二进制并安装，自动生成配置模板与 systemd 单元。
#
# 用法示例:
#   curl -fsSL https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh | sh
#   sh install.sh --version v0.1.0 --install-dir ~/.local/bin --no-systemd
#   sh install.sh --from-source          # 本机有 Rust 时从源码编译
#   sh install.sh --uninstall            # 卸载
set -eu

BIN="matrix-satori"
REPO="FLY1919/matrix-satori"
API_BASE="https://api.github.com"
DOWNLOAD_BASE="https://github.com"
VERSION=""
INSTALL_DIR=""
CONFIG_DIR=""
SYSTEMD="auto"          # auto | yes | no
FROM_SOURCE=0
UNINSTALL=0
ASSUME_YES=0
HOMESERVER="https://mozilla.modular.im"
LOGIN_METHOD="sso"
SATORI_BIND="127.0.0.1:5140"
SATORI_TOKEN=""

say()  { printf '%s\n' "$*"; }
fail() { printf '错误: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<EOF
matrix-satori 一键安装

用法: sh install.sh [选项]

选项:
  -v, --version VER       指定版本 tag，默认自动取最新 Release
      --install-dir DIR   二进制安装目录 (默认: root=/usr/local/bin, 否则 ~/.local/bin)
      --config-dir DIR    配置目录 (默认: root=/etc/matrix-satori, 否则 ~/.config/matrix-satori)
      --systemd           强制生成并安装 systemd 单元
      --no-systemd        不生成 systemd 单元
      --homeserver URL    Matrix homeserver (默认 $HOMESERVER)
      --login-method M    auto/password/token/sso/applicationservice/accesstoken (默认 $LOGIN_METHOD)
      --bind ADDR         Satori 监听地址 (默认 $SATORI_BIND)
      --token TOKEN       Satori 共享密钥 (默认随机生成)
  -y, --yes               跳过交互提示
      --from-source       忽略 Release，直接用 cargo 从源码编译
      --uninstall         卸载已安装的二进制与 systemd 单元
      --repo OWNER/NAME   仓库 (默认 $REPO)
      --api-base URL      GitHub API 地址 (默认 $API_BASE)
      --download-base URL Release 下载基址 (默认 $DOWNLOAD_BASE)
  -h, --help              显示本帮助
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    -v|--version)       VERSION="${2:?--version 需要值}"; shift 2 ;;
    --install-dir)      INSTALL_DIR="${2:?--install-dir 需要值}"; shift 2 ;;
    --config-dir)       CONFIG_DIR="${2:?--config-dir 需要值}"; shift 2 ;;
    --systemd)          SYSTEMD=yes; shift ;;
    --no-systemd)       SYSTEMD=no; shift ;;
    --homeserver)       HOMESERVER="${2:?--homeserver 需要值}"; shift 2 ;;
    --login-method)     LOGIN_METHOD="${2:?--login-method 需要值}"; shift 2 ;;
    --bind)             SATORI_BIND="${2:?--bind 需要值}"; shift 2 ;;
    --token)            SATORI_TOKEN="${2:?--token 需要值}"; shift 2 ;;
    -y|--yes)           ASSUME_YES=1; shift ;;
    --from-source)      FROM_SOURCE=1; shift ;;
    --uninstall)        UNINSTALL=1; shift ;;
    --repo)             REPO="${2:?--repo 需要值}"; shift 2 ;;
    --api-base)         API_BASE="${2:?--api-base 需要值}"; shift 2 ;;
    --download-base)    DOWNLOAD_BASE="${2:?--download-base 需要值}"; shift 2 ;;
    -h|--help)          usage; exit 0 ;;
    *) fail "未知参数: $1 (用 --help 查看用法)" ;;
  esac
done

is_root() { [ "$(id -u)" = "0" ]; }

SUDO=""
need_sudo() {
  [ -n "$SUDO" ] && return 0
  is_root && return 0
  command -v sudo >/dev/null 2>&1 && SUDO="sudo"
  return 0
}

http_get() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    fail "需要 curl 或 wget"
  fi
}

http_download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    fail "需要 curl 或 wget"
  fi
}

json_field() {
  key="$1"; json="$2"
  if command -v python3 >/dev/null 2>&1; then
    printf '%s' "$json" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get(sys.argv[1], ""))' "$key"
  else
    printf '%s' "$json" | sed -n "s/.*\"$key\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1
  fi
}

# 检测平台，设置 TARGET 与 EXT
detect_target() {
  os=$(uname -s); arch=$(uname -m)
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64)   TARGET="x86_64-unknown-linux-musl" ;;
        aarch64|arm64)  TARGET="aarch64-unknown-linux-musl" ;;
        armv7*|armv6*)  TARGET="armv7-unknown-linux-musleabihf" ;;
        *) fail "不支持的 CPU 架构: $arch" ;;
      esac ;;
    Darwin)
      case "$arch" in
        x86_64)         TARGET="x86_64-apple-darwin" ;;
        arm64|aarch64)  TARGET="aarch64-apple-darwin" ;;
        *) fail "不支持的 CPU 架构: $arch" ;;
      esac ;;
    MINGW*|MSYS*|CYGWIN*)
      case "$arch" in
        x86_64|amd64)   TARGET="x86_64-pc-windows-msvc" ;;
        *) fail "不支持的 CPU 架构: $arch" ;;
      esac ;;
    *) fail "不支持的操作系统: $os" ;;
  esac
  case "$TARGET" in
    *windows*) EXT="zip" ;;
    *)         EXT="tar.gz" ;;
  esac
  say "目标平台: $TARGET ($EXT)"
}

resolve_version() {
  [ -n "$VERSION" ] && return 0
  say "正在查询最新 Release ..."
  meta=$(http_get "$API_BASE/repos/$REPO/releases/latest") || fail "无法查询 Release 信息"
  VERSION=$(json_field tag_name "$meta")
  [ -n "$VERSION" ] || fail "解析不到最新版本号"
  say "最新版本: $VERSION"
}

verify_checksum() {
  file="$1"; sumfile="$2"; dir=$(dirname "$file"); base=$(basename "$file"); sbase=$(basename "$sumfile")
  ( cd "$dir" && \
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "$sbase"
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 -c "$sbase"
    elif command -v openssl >/dev/null 2>&1; then
      want=$(awk '{print $1}' "$sbase")
      got=$(openssl dgst -sha256 "$base" | awk '{print $NF}')
      [ "$want" = "$got" ] || return 1
    else
      echo "没有可用的 SHA256 校验工具" >&2; return 1
    fi
  ) || fail "SHA256 校验失败，安装已中止"
  say "SHA256 校验通过"
}

install_binary() {
  src="$1"
  need_sudo
  $SUDO mkdir -p "$INSTALL_DIR"
  [ -x "$src" ] || chmod +x "$src"
  dest="$INSTALL_DIR/$BIN"
  if command -v install >/dev/null 2>&1; then
    $SUDO install -m 0755 "$src" "$dest"
  else
    $SUDO cp "$src" "$dest" && $SUDO chmod 0755 "$dest"
  fi
  BIN_PATH="$dest"
  say "已安装二进制: $dest"
}

install_from_release() {
  detect_target
  ASSET="${BIN}_${VERSION}_${TARGET}.${EXT}"
  URL="${DOWNLOAD_BASE}/${REPO}/releases/download/${VERSION}/${ASSET}"
  say "下载: $URL"
  http_download "$URL" "$TMP/$ASSET" || fail "下载失败，请检查版本号与网络连接"
  say "下载校验文件 ..."
  http_download "${URL}.sha256" "$TMP/${ASSET}.sha256" || fail "下载 .sha256 失败"
  verify_checksum "$TMP/$ASSET" "$TMP/${ASSET}.sha256"

  say "解压 ..."
  mkdir -p "$TMP/extract"
  if [ "$EXT" = "zip" ]; then
    if command -v unzip >/dev/null 2>&1; then
      unzip -q "$TMP/$ASSET" -d "$TMP/extract"
    else
      python3 -c 'import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$TMP/$ASSET" "$TMP/extract"
    fi
  else
    tar -xzf "$TMP/$ASSET" -C "$TMP/extract"
  fi
  src=$(find "$TMP/extract" -type f \( -name "$BIN" -o -name "$BIN.exe" \) | head -1)
  [ -n "$src" ] || fail "解压后找不到二进制 $BIN"
  install_binary "$src"
}

install_from_source() {
  command -v cargo >/dev/null 2>&1 || fail "--from-source 需要本机安装 Rust (cargo)"
  command -v git >/dev/null 2>&1 || fail "--from-source 需要 git"
  say "从源码编译（可能需要几分钟）..."
  git clone --depth 1 "https://github.com/$REPO.git" "$TMP/src"
  cargo build --release --manifest-path "$TMP/src/Cargo.toml"
  install_binary "$TMP/src/target/release/$BIN"
}

random_token() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 24
  else
    od -An -tx1 -N24 /dev/urandom 2>/dev/null | tr -d ' \n'
  fi
}

write_config() {
  [ -n "$SATORI_TOKEN" ] || SATORI_TOKEN=$(random_token)
  [ -n "$SATORI_TOKEN" ] || fail "无法生成随机密钥，请用 --token 指定"
  need_sudo
  $SUDO mkdir -p "$CONFIG_DIR"
  ENV_FILE="$CONFIG_DIR/matrix-satori.env"
  STATE_DIR="$CONFIG_DIR/data"

  if [ -e "$ENV_FILE" ]; then
    say "保留已有配置: $ENV_FILE"
    # 读取已有 token，供结尾提示使用
    old=$(sed -n 's/^SATORI_TOKEN=//p' "$ENV_FILE" | head -1)
    [ -n "$old" ] && SATORI_TOKEN="$old"
  else
    cat > "$TMP/env.tmp" <<EOF
# matrix-satori 配置（由 install.sh 生成，可按需修改）
# Matrix homeserver 与登录方式
MATRIX_HOMESERVER=$HOMESERVER
MATRIX_LOGIN_METHOD=$LOGIN_METHOD

# 密码登录时填写下面两项:
# MATRIX_USERNAME=
# MATRIX_PASSWORD=
# 已有 access token 登录时填写（留空则按登录方式交互获取）:
# MATRIX_ACCESS_TOKEN=
# SSO 登录的本地回调地址（SSO 时程序会打印登录链接）:
MATRIX_SSO_CALLBACK_BIND=127.0.0.1:8088

# Satori 服务（Koishi 连接用）
SATORI_BIND=$SATORI_BIND
SATORI_TOKEN=$SATORI_TOKEN

# 凭据缓存与同步位点目录
MATRIX_STATE_DIR=$STATE_DIR
RUST_LOG=info
EOF
    $SUDO cp "$TMP/env.tmp" "$ENV_FILE"
    $SUDO chmod 0600 "$ENV_FILE"
    say "已生成配置: $ENV_FILE"
  fi
  $SUDO mkdir -p "$STATE_DIR" 2>/dev/null || true
}

write_systemd() {
  [ "$SYSTEMD" = "yes" ] || return 0
  if ! command -v systemctl >/dev/null 2>&1; then
    say "未检测到 systemctl，跳过 systemd 单元"
    return 0
  fi
  UNIT_SRC="$CONFIG_DIR/matrix-satori.service"
  cat > "$TMP/unit.tmp" <<EOF
[Unit]
Description=matrix-satori (Matrix -> Satori bridge for Koishi)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=$ENV_FILE
ExecStart=$BIN_PATH
Restart=on-failure
RestartSec=5
WorkingDirectory=$CONFIG_DIR

[Install]
WantedBy=multi-user.target
EOF
  $SUDO cp "$TMP/unit.tmp" "$UNIT_SRC"
  if is_root && [ -d /etc/systemd/system ]; then
    $SUDO cp "$UNIT_SRC" /etc/systemd/system/matrix-satori.service
    systemctl daemon-reload >/dev/null 2>&1 || true
    say "已安装 systemd 单元: /etc/systemd/system/matrix-satori.service"
    say "启用并启动: systemctl enable --now matrix-satori"
  else
    say "systemd 单元模板已生成: $UNIT_SRC"
    say "如需启用，复制到 /etc/systemd/system/ 后执行 systemctl daemon-reload && systemctl enable --now matrix-satori"
  fi
}

do_uninstall() {
  need_sudo
  if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now matrix-satori >/dev/null 2>&1 || true
  fi
  $SUDO rm -f /etc/systemd/system/matrix-satori.service 2>/dev/null || true
  command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload >/dev/null 2>&1 || true
  $SUDO rm -f "$INSTALL_DIR/$BIN" 2>/dev/null || true
  $SUDO rm -f "$CONFIG_DIR/matrix-satori.service" 2>/dev/null || true
  say "已卸载 $INSTALL_DIR/$BIN 与 systemd 单元"
  say "配置与数据目录 $CONFIG_DIR 已保留（如需删除请手动 rm -rf）"
}

main() {
  if [ -z "$INSTALL_DIR" ]; then
    if is_root; then INSTALL_DIR="/usr/local/bin"; else INSTALL_DIR="$HOME/.local/bin"; fi
  fi
  if [ -z "$CONFIG_DIR" ]; then
    if is_root; then CONFIG_DIR="/etc/matrix-satori"; else CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/matrix-satori"; fi
  fi
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT HUP INT TERM

  if [ "$UNINSTALL" -eq 1 ]; then
    do_uninstall
    return 0
  fi

  if [ "$SYSTEMD" = "auto" ]; then
    if is_root && command -v systemctl >/dev/null 2>&1; then SYSTEMD=yes; else SYSTEMD=no; fi
  fi

  if [ "$FROM_SOURCE" -eq 1 ]; then
    install_from_source
  else
    resolve_version
    install_from_release
  fi

  write_config
  write_systemd

  say ""
  say "安装完成!"
  say "  二进制:   $BIN_PATH"
  say "  配置:     $CONFIG_DIR/matrix-satori.env"
  say "  数据目录: $CONFIG_DIR/data"
  say "  Koishi:   adapter-satori 的 endpoint 填 $SATORI_BIND 对应的地址，token 填: $SATORI_TOKEN"
  if [ "$SYSTEMD" = "yes" ]; then
    say "  启动:     systemctl enable --now matrix-satori"
  else
    say "  启动:     $BIN_PATH   (会自动读取 $CONFIG_DIR/matrix-satori.env)"
  fi
}

main "$@"
