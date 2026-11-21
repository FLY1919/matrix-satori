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
MIRROR="auto"           # auto | cn | direct  —— GitHub 访问模式
CONNECT_TIMEOUT=8       # 连接超时(秒)：单次 HTTP 请求连接阶段上限
DOWNLOAD_TIMEOUT=180    # 单个下载最大时长(秒)
PROBE_TIMEOUT=5         # 测速探测上限(秒)：并发探测各源，超时仅用于封顶，不决定优先级
FROM_SOURCE=0
UNINSTALL=0
ASSUME_YES=0
DOCTOR=0
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
      --mirror MODE       GitHub 访问模式: auto(加速+直连一起测速,默认) / cn(只测国内加速) / direct(只直连)
                          安装前会并发探测各源响应速度，按最快优先，而不是按固定顺序+超时试错
                          国内网络慢时推荐 --mirror cn；也可用环境变量 MATRIX_SATORI_MIRROR
      --connect-timeout N 连接超时秒数 (默认 $CONNECT_TIMEOUT)
      --probe-timeout N   测速探测上限秒数 (默认 $PROBE_TIMEOUT)，仅用于给慢源封顶
      --doctor            自检：工具/平台/仓库 Release 状态/各源实测延迟排名，不安装
  -h, --help              显示本帮助
EOF
}

# 环境变量可预设镜像模式
[ -n "${MATRIX_SATORI_MIRROR:-}" ] && MIRROR="$MATRIX_SATORI_MIRROR"

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
    --mirror)           MIRROR="${2:?--mirror 需要值}"; shift 2 ;;
    --connect-timeout)  CONNECT_TIMEOUT="${2:?--connect-timeout 需要值}"; shift 2 ;;
    --probe-timeout)    PROBE_TIMEOUT="${2:?--probe-timeout 需要值}"; shift 2 ;;
    --doctor)           DOCTOR=1; shift ;;
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

# 所有网络请求都带连接超时与总时长上限，避免国内直连 GitHub 时无限卡住。
# 失败时清理半截文件，保证下载回退源不会把残缺包当成成功。
http_get() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout "$CONNECT_TIMEOUT" --max-time "$DOWNLOAD_TIMEOUT" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- --timeout="$CONNECT_TIMEOUT" --tries=1 "$1"
  else
    fail "需要 curl 或 wget"
  fi
}

http_download() {
  dest="$2"
  if command -v curl >/dev/null 2>&1; then
    ok=$(curl -fsSL --connect-timeout "$CONNECT_TIMEOUT" --max-time "$DOWNLOAD_TIMEOUT" -o "$dest" "$1" 2>/dev/null && echo 1 || echo 0)
  elif command -v wget >/dev/null 2>&1; then
    ok=$(wget -q -O "$dest" --timeout="$CONNECT_TIMEOUT" --tries=1 "$1" 2>/dev/null && echo 1 || echo 0)
  else
    fail "需要 curl 或 wget"
  fi
  [ "$ok" = "1" ] || { rm -f "$dest"; return 1; }
  return 0
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
# MACOS=1 表示当前是 macOS：CI 不再产出 apple-darwin 预编译包，
# 因此这类平台只能走 --from-source 编译。
MACOS=0
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
      MACOS=1
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

# 统一的镜像候选列表，供 API 查询 / 资产下载 / 自检共用。
# 每个条目格式: "显示名|API基址|下载基址"
# 注意：这里的顺序只是候选，不代表优先级；实际优先级由下面的并发测速决定。
# MIRROR 模式决定哪些源参与测速:
#   auto   —— 国内加速 + 官方直连一起测速，谁快用谁（默认）
#   cn     —— 只在国内加速源里测速
#   direct —— 只在官方直连里测速（只有一个候选，无需测速）
mirror_entries() {
  cn_list="\
gh-proxy|https://gh-proxy.com/https://api.github.com|https://gh-proxy.com/https://github.com
ghproxy.net|https://ghproxy.net/https://api.github.com|https://ghproxy.net/https://github.com
ghfast.top||https://ghfast.top/https://github.com
gitmirror|https://hub.gitmirror.com/https://api.github.com|https://hub.gitmirror.com/https://github.com"
  direct_list="\
github|https://api.github.com|https://github.com"
  case "$MIRROR" in
    cn)     printf '%s\n' "$cn_list" ;;
    direct) printf '%s\n' "$direct_list" ;;
    *)      printf '%s\n%s\n' "$cn_list" "$direct_list" ;;   # auto: 两类源都参与测速
  esac
}

# ---- 并发测速选源 ----
# API 与下载走的是不同的 URL 路径，可达性/速度可能不同，因此分开探测、各自排序：
#   - API：探测 <api>/repos/OWNER/REPO/releases/latest（贴近真实用途）
#   - 下载：探测 <dl>/OWNER/REPO（仓库首页，assets 同域）
# 每个候选后台并发请求，用 curl 的 time_total 作为延迟，按快→慢排序并缓存。
# 这样"谁快用谁"，而不是按写死的顺序 + 超时试错。超时只给探测封顶，不决定优先级。
RANKED_API=""     # 排序后的 API 基址（换行分隔）
RANKED_DL=""      # 排序后的下载基址（换行分隔）
PROBE_API_SORTED=""
PROBE_DL_SORTED=""

# 探测单个 URL 的响应延迟（秒），失败/超时返回 999999
probe_latency() {
  url="$1"; latency=""
  if command -v curl >/dev/null 2>&1; then
    latency=$(curl -s -o /dev/null -w '%{time_total}' \
      --connect-timeout "$PROBE_TIMEOUT" --max-time "$PROBE_TIMEOUT" "$url" 2>/dev/null) || latency=""
  elif command -v wget >/dev/null 2>&1; then
    s=$(date +%s%N 2>/dev/null || echo 0)
    wget -q -O /dev/null --timeout="$PROBE_TIMEOUT" --tries=1 "$url" 2>/dev/null || true
    e=$(date +%s%N 2>/dev/null || echo 0)
    if [ "$s" != "0" ] && [ "$e" != "0" ]; then
      latency=$(awk -v a="$s" -v b="$e" 'BEGIN{printf "%.3f",(b-a)/1000000000}')
    fi
  fi
  case "$latency" in
    ''|*[!0-9.]*) latency="999999" ;;
  esac
  printf '%s' "$latency"
}

rank_mirrors() {
  [ -n "$RANKED_API" ] && [ -n "$RANKED_DL" ] && return 0
  say "并发探测镜像延迟（超时上限 ${PROBE_TIMEOUT}s，API/下载分别测速，快→慢）..."
  pdir=$(mktemp -d)
  old_ifs=$IFS; IFS='
'
  for line in $(mirror_entries); do
    IFS=$old_ifs
    [ -z "$line" ] && { IFS='
'; continue; }
    name=$(printf '%s' "$line" | cut -d'|' -f1)
    api=$(printf '%s' "$line" | cut -d'|' -f2)
    dl=$(printf '%s' "$line" | cut -d'|' -f3)
    if [ -n "$api" ]; then
      ( lat=$(probe_latency "$api/repos/$REPO/releases/latest"); printf '%s|%s\n' "$lat" "$api" > "$pdir/api.$name" ) &
    fi
    if [ -n "$dl" ]; then
      ( lat=$(probe_latency "$dl/$REPO"); printf '%s|%s\n' "$lat" "$dl" > "$pdir/dl.$name" ) &
    fi
    IFS='
'
  done
  IFS=$old_ifs
  wait
  PROBE_API_SORTED=$(cat "$pdir"/api.* 2>/dev/null | sort -t'|' -k1,1n)
  PROBE_DL_SORTED=$(cat "$pdir"/dl.* 2>/dev/null | sort -t'|' -k1,1n)
  rm -rf "$pdir"
  RANKED_API=$(printf '%s\n' "$PROBE_API_SORTED" | awk -F'|' 'NF>=2 {print $2}')
  RANKED_DL=$(printf '%s\n' "$PROBE_DL_SORTED" | awk -F'|' 'NF>=2 {print $2}')
  # 打印两份排名，让用户看清为什么选了这个源
  say "  API 源:"
  old_ifs=$IFS; IFS='
'
  for line in $PROBE_API_SORTED; do
    IFS=$old_ifs; lat=$(printf '%s' "$line" | cut -d'|' -f1); url=$(printf '%s' "$line" | cut -d'|' -f2)
    [ "$lat" = "999999" ] && say "    - 超时  $url" || say "    - ${lat}s  $url"
    IFS='
'
  done
  say "  下载源:"
  for line in $PROBE_DL_SORTED; do
    IFS=$old_ifs; lat=$(printf '%s' "$line" | cut -d'|' -f1); url=$(printf '%s' "$line" | cut -d'|' -f2)
    [ "$lat" = "999999" ] && say "    - 超时  $url" || say "    - ${lat}s  $url"
    IFS='
'
  done
  IFS=$old_ifs
}

# 校验 mirror 模式，写错时早点报错而不是下载到一半才发现
validate_mirror() {
  case "$MIRROR" in
    auto|cn|direct) ;;
    *) fail "无效的 --mirror 值: $MIRROR (可选 auto/cn/direct)" ;;
  esac
}

# 用户显式改过基址时优先用它，否则按对应类别的测速排名（谁快用谁）
api_sources() {
  rank_mirrors
  if [ "$API_BASE" != "https://api.github.com" ]; then
    printf '%s\n' "$API_BASE"
  fi
  printf '%s\n' "$RANKED_API"
}

download_bases() {
  rank_mirrors
  if [ "$DOWNLOAD_BASE" != "https://github.com" ]; then
    printf '%s\n' "$DOWNLOAD_BASE"
  fi
  printf '%s\n' "$RANKED_DL"
}

resolve_version() {
  [ -n "$VERSION" ] && return 0
  say "正在查询最新 Release ..."
  got=""
  src_list=$(api_sources)
  old_ifs=$IFS; IFS='
'
  for base in $src_list; do
    IFS=$old_ifs
    [ -z "$base" ] && { IFS='
'; continue; }
    say "  查询 API: $base"
    if meta=$(http_get "$base/repos/$REPO/releases/latest" 2>/dev/null); then
      v=$(json_field tag_name "$meta")
      if [ -n "$v" ]; then got="$v"; say "  ✓ 命中: $base"; break; fi
    fi
    IFS='
'
  done
  IFS=$old_ifs
  if [ -z "$got" ]; then
    cat >&2 <<EOF
错误: 查询不到最新 Release。

这通常意味着仓库还没有发布任何版本（releases/latest 会返回 404），
也可能是网络无法访问 GitHub API。你可以：

  1) 用源码编译安装（本机需有 Rust）:
       sh install.sh --from-source
  2) 稍后重试（等维护者 git tag 触发 Release workflow 完成）
  3) 自检定位问题:
       sh install.sh --doctor
  4) 显式指定版本:
       sh install.sh --version v0.1.0
EOF
    exit 1
  fi
  VERSION="$got"
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
  # 保留平台后缀：Windows 下 src 是 matrix-satori.exe，目标也必须带 .exe
  dest="$INSTALL_DIR/$(basename "$src")"
  if command -v install >/dev/null 2>&1; then
    $SUDO install -m 0755 "$src" "$dest"
  else
    $SUDO cp "$src" "$dest" && $SUDO chmod 0755 "$dest"
  fi
  BIN_PATH="$dest"
  say "已安装二进制: $dest"
}

# 下载源回退：按镜像优先级逐个尝试，成功即停
download_asset() {
  # $1=相对路径(含 releases/download/...)，$2=输出文件
  rel="$1"; out="$2"; ok=0
  old_ifs=$IFS; IFS='
'
  for base in $(download_bases); do
    IFS=$old_ifs
    [ -z "$base" ] && { IFS='
'; continue; }
    say "  尝试下载源: $base"
    if http_download "${base}/${rel}" "$out"; then ok=1; break; fi
    IFS='
'
  done
  IFS=$old_ifs
  [ "$ok" -eq 1 ]
}

install_from_release() {
  detect_target
  if [ "$MACOS" -eq 1 ]; then
    cat >&2 <<EOF
提示: 当前为 macOS。本项目不再提供 macOS 预编译包（CI 已移除 macOS 构建），
      因此无法一键下载。请在 macOS 上用源码编译安装（需本机有 Rust）:

        sh install.sh --from-source

      若只是想要一个能在 Mac 上跑的二进制，也可以自行 cargo build --release。
EOF
    exit 1
  fi
  ASSET="${BIN}_${VERSION}_${TARGET}.${EXT}"
  REL="${REPO}/releases/download/${VERSION}/${ASSET}"
  say "下载资产: $REL  (镜像模式: $MIRROR)"
  download_asset "$REL" "$TMP/$ASSET" || fail "下载失败（已尝试全部镜像源），可试 --mirror cn / --doctor 自检 / --from-source 编译"
  say "下载校验文件 ..."
  download_asset "$REL.sha256" "$TMP/${ASSET}.sha256" || fail "下载 .sha256 失败（已尝试全部镜像源）"
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
  # git 克隆按镜像模式选源：cn 优先加速，direct 只直连
  cloned=0
  old_ifs=$IFS; IFS='
'
  for base in $(download_bases); do
    IFS=$old_ifs
    [ -z "$base" ] && { IFS='
'; continue; }
    say "  git clone via $base"
    if git clone --depth 1 "$base/$REPO.git" "$TMP/src" 2>/dev/null; then cloned=1; break; fi
    IFS='
'
  done
  IFS=$old_ifs
  [ "$cloned" -eq 1 ] || fail "git clone 失败（已尝试全部镜像源）"
  # 项目自带 .cargo/config.toml(rsproxy 源)，国内 cargo 下载也会加速
  cargo build --release --manifest-path "$TMP/src/Cargo.toml" || fail "cargo build 失败"
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
  $SUDO rm -f "$INSTALL_DIR/$BIN" "$INSTALL_DIR/$BIN.exe" 2>/dev/null || true
  $SUDO rm -f "$CONFIG_DIR/matrix-satori.service" 2>/dev/null || true
  say "已卸载 $INSTALL_DIR/$BIN 与 systemd 单元"
  say "配置与数据目录 $CONFIG_DIR 已保留（如需删除请手动 rm -rf）"
}

# 自检：只诊断不安装
do_doctor() {
  say "=== matrix-satori 环境自检 ==="
  say ""
  say "[1/4] 必需工具"
  for t in curl sh tar; do
    if command -v "$t" >/dev/null 2>&1; then say "  OK  $t"; else say "  缺失 $t"; fi
  done
  for t in wget unzip python3 sha256sum shasum openssl cargo git; do
    if command -v "$t" >/dev/null 2>&1; then say "  OK  $t ($(command -v "$t"))"; else say "  --  $t (未安装，非必需)"; fi
  done
  say ""
  say "[2/4] 平台识别"
  detect_target 2>/dev/null || say "  未能识别当前平台"
  if [ "$MACOS" -eq 1 ]; then
    say "  注意: macOS 无预编译包，请用 --from-source 从源码编译"
  fi
  say ""
  say "[3/4] 仓库 Release 状态"
  found=0
  old_ifs=$IFS; IFS='
'
  for base in $(api_sources); do
    IFS=$old_ifs
    [ -z "$base" ] && continue
    if meta=$(http_get "$base/repos/$REPO/releases/latest" 2>/dev/null); then
      v=$(json_field tag_name "$meta")
      if [ -n "$v" ]; then
        say "  最新 Release: $v  (via $base)"
        count=$(printf '%s' "$meta" | tr ',' '\n' | grep -c '"name"' || true)
        say "  资产数量(粗略): $count"
        found=1; break
      fi
    fi
    IFS='
'
  done
  IFS=$old_ifs
  if [ "$found" -eq 0 ]; then
    say "  未找到 Release —— 仓库可能还没发版，请用 --from-source 编译安装"
  fi
  say ""
  say "[4/4] 可用性汇总（模式: $MIRROR，明细见开头排名）"
  api_ok=$(printf '%s\n' "$PROBE_API_SORTED" | grep -vc '^999999|' || true)
  dl_ok=$(printf '%s\n' "$PROBE_DL_SORTED" | grep -vc '^999999|' || true)
  say "  API 源可用: $api_ok   下载源可用: $dl_ok"
  [ "$api_ok" -gt 0 ] && [ "$dl_ok" -gt 0 ] || say "  部分类型全部不可达 —— 试试 --mirror cn 或配置网络代理"
  say ""
  say "诊断完成。若 Release 状态为'未找到'，先等维护者发版，或用 --from-source。"
}

main() {
  validate_mirror
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

  # 在父 shell 里先测一次速并缓存。后续 api_sources/download_bases 都在 $(...) 子 shell
  # 里调用，子进程会继承这里的 RANKED 缓存而直接复用，避免重复探测。
  rank_mirrors

  if [ "$DOCTOR" -eq 1 ]; then
    do_doctor
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
