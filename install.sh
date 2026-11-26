#!/usr/bin/env sh
# matrix-satori 一键安装
#   curl -fsSL https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh | sh
#   sh install.sh --version v0.2.11 --no-systemd --mirror cn
set -eu

BIN=matrix-satori
REPO=FLY1919/matrix-satori
VERSION=${VERSION:-}
INSTALL_DIR=${INSTALL_DIR:-}
CONFIG_DIR=${CONFIG_DIR:-}
SYSTEMD=auto
MIRROR=${MATRIX_SATORI_MIRROR:-auto}      # auto | cn | direct
HOMESERVER=${HOMESERVER:-https://mozilla.modular.im}
LOGIN_METHOD=${LOGIN_METHOD:-sso}
BIND=${BIND:-127.0.0.1:5140}
TOKEN=${TOKEN:-}
FROM_SOURCE=0
UNINSTALL=0
DOCTOR=0
PROBE=${PROBE_TIMEOUT:-5}
DL_TIMEOUT=180

say()  { printf '%s\n' "$*"; }
die()  { printf '错误: %s\n' "$*" >&2; exit 1; }
root() { [ "$(id -u)" = 0 ]; }
sudo() { if root; then "$@"; elif command -v sudo >/dev/null; then command sudo "$@"; else die "需要 root 或 sudo"; fi; }

usage() { cat <<EOF
matrix-satori 一键安装
用法: sh install.sh [选项]
  -v, --version V      指定版本 tag (默认取最新 Release)
      --install-dir D  二进制目录 (root:/usr/local/bin 否则 ~/.local/bin)
      --config-dir D   配置目录 (root:/etc/matrix-satori 否则 ~/.config/matrix-satori)
      --systemd | --no-systemd   是否安装 systemd 单元 (默认自动)
      --homeserver U   Matrix homeserver (默认 $HOMESERVER)
      --login-method M auto/password/token/sso/appservice/accesstoken
      --bind ADDR      Satori 监听地址 (默认 $BIND)
      --token T        Satori 共享密钥 (默认随机生成)
      --mirror M       auto(加速+直连测速,默认) / cn(仅加速) / direct(仅直连)
      --probe-timeout N 测速上限秒数 (默认 $PROBE)
      --from-source    源码编译 (需 cargo)
      --uninstall      卸载
      --doctor         环境自检，不安装
  -h, --help
EOF
}

while [ $# -gt 0 ]; do case $1 in
  -v|--version)      VERSION=$2; shift 2 ;;
  --install-dir)     INSTALL_DIR=$2; shift 2 ;;
  --config-dir)      CONFIG_DIR=$2; shift 2 ;;
  --systemd)         SYSTEMD=yes; shift ;;
  --no-systemd)      SYSTEMD=no; shift ;;
  --homeserver)      HOMESERVER=$2; shift 2 ;;
  --login-method)    LOGIN_METHOD=$2; shift 2 ;;
  --bind)            BIND=$2; shift 2 ;;
  --token)           TOKEN=$2; shift 2 ;;
  --mirror)          MIRROR=$2; shift 2 ;;
  --probe-timeout)   PROBE=$2; shift 2 ;;
  --from-source)     FROM_SOURCE=1; shift ;;
  --uninstall)       UNINSTALL=1; shift ;;
  --doctor)          DOCTOR=1; shift ;;
  -h|--help)         usage; exit 0 ;;
  *) die "未知参数: $1 (--help 查看用法)" ;;
esac; done
case $MIRROR in auto|cn|direct) ;; *) die "--mirror 只能是 auto/cn/direct" ;; esac

# ---- 网络小工具 ----
get() { if command -v curl >/dev/null; then curl -fsSL --connect-timeout 10 --max-time "$DL_TIMEOUT" "$1"
        elif command -v wget >/dev/null; then wget -qO- --timeout=10 "$1"; else die "需要 curl 或 wget"; fi; }
fetch() { if command -v curl >/dev/null; then curl -fsSL --connect-timeout 10 --max-time "$DL_TIMEOUT" -o "$2" "$1" 2>/dev/null
          else wget -q -O "$2" --timeout=10 "$1" 2>/dev/null; fi || { rm -f "$2"; return 1; }; }
field() { if command -v python3 >/dev/null; then printf '%s' "$2" | python3 -c 'import json,sys;print(json.load(sys.stdin).get(sys.argv[1],""))' "$1"
          else printf '%s' "$2" | sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1; fi; }

# ---- 平台 ----
detect() {
  case $(uname -s) in
    Linux) case $(uname -m) in
      x86_64|amd64)  T=x86_64-unknown-linux-musl ;;
      aarch64|arm64) T=aarch64-unknown-linux-musl ;;
      armv7*|armv6*) T=armv7-unknown-linux-musleabihf ;;
      *) die "不支持架构: $(uname -m)" ;;
    esac; EXT=tar.gz ;;
    Darwin) die "macOS 无预编译包，请用 --from-source 源码编译" ;;
    MINGW*|MSYS*|CYGWIN*) T=x86_64-pc-windows-msvc; EXT=zip ;;
    *) die "不支持系统: $(uname -s)" ;;
  esac
  say "平台: $T"
}

# ---- 镜像源 + 并发测速 (API / 下载分开排名，谁快用谁) ----
sources() {
  cn="gh-proxy|https://gh-proxy.com/https://api.github.com|https://gh-proxy.com/https://github.com
ghproxy|https://ghproxy.net/https://api.github.com|https://ghproxy.net/https://github.com
gitmirror|https://hub.gitmirror.com/https://api.github.com|https://hub.gitmirror.com/https://github.com"
  direct="github|https://api.github.com|https://github.com"
  case $MIRROR in cn) printf '%s\n' "$cn" ;; direct) printf '%s\n' "$direct" ;; *) printf '%s\n%s\n' "$cn" "$direct" ;; esac
}
latency() { # URL -> 秒，失败 999999
  if command -v curl >/dev/null; then l=$(curl -s -o /dev/null -w '%{time_total}' --connect-timeout "$PROBE" --max-time "$PROBE" "$1" 2>/dev/null) || l=
  else s=$(date +%s%N 2>/dev/null || echo 0); wget -q -O /dev/null --timeout="$PROBE" "$1" 2>/dev/null || :; e=$(date +%s%N 2>/dev/null || echo 0)
       l=$(awk -v a=$s -v b=$e 'BEGIN{printf "%.3f",(b-a)/1000000000}'); fi
  case $l in ''|*[!0-9.]*) l=999999 ;; esac; printf '%s' "$l"
}
RANK_API='' RANK_DL=''
rank() {
  [ -n "$RANK_DL" ] && return 0
  say "并发测速 (上限 ${PROBE}s, 快→慢):"
  d=$(mktemp -d)
  old=$IFS; IFS='
'
  for line in $(sources); do IFS=$old
    n=$(printf %s "$line" | cut -d'|' -f1); api=$(printf %s "$line" | cut -d'|' -f2); dl=$(printf %s "$line" | cut -d'|' -f3)
    [ -n "$api" ] && ( printf '%s|%s\n' "$(latency "$api/repos/$REPO/releases/latest")" "$api" > "$d/a.$n" ) &
    [ -n "$dl" ]  && ( printf '%s|%s\n' "$(latency "$dl/$REPO")" "$dl" > "$d/d.$n" ) &
    IFS='
'
  done
  IFS=$old; wait
  RANK_API=$(cat "$d"/a.* 2>/dev/null | sort -t'|' -k1,1n | awk -F'|' 'NF>1{print $2}')
  RANK_DL=$(cat  "$d"/d.* 2>/dev/null | sort -t'|' -k1,1n | awk -F'|' 'NF>1{print $2}')
  cat "$d"/a.* "$d"/d.* 2>/dev/null | sort -t'|' -k1,1n | while IFS='|' read -r l u; do
    [ "$l" = 999999 ] && say "  超时  $u" || say "  ${l}s  $u"; done
  rm -rf "$d"
}
api_list() { rank; printf '%s\n' "$RANK_API"; }
dl_list()  { rank; printf '%s\n' "$RANK_DL"; }
each() { # each <列表函数> <命令...>: 把每个源作为参数依次尝试，成功即停
  _fn=$1; shift
  old=$IFS; IFS='
'; ok=0
  for b in $($_fn); do IFS=$old; [ -z "$b" ] && { IFS='
'; continue; }
    if "$@" "$b"; then ok=1; break; fi
    IFS='
'
  done
  IFS=$old; [ "$ok" -eq 1 ]
}

# ---- 版本 / 下载 / 安装 ----
latest() { meta=$(get "$1/repos/$REPO/releases/latest" 2>/dev/null) || return 1
  v=$(field tag_name "$meta"); [ -n "$v" ] && { VERSION=$v; say "最新版: $v"; return 0; }; return 1; }
resolve_version() { [ -n "$VERSION" ] && return 0; say "查询最新 Release ..."
  each api_list latest || die "查不到 Release: 用 --from-source 编译 / --version 指定 / --doctor 自检"; }

sha_verify() { # 资产文件 + 汇总校验文件路径 (SHA256SUMS)，按文件名取该行校验
  f=$1; sums=$2; b=$(basename "$f")
  line=$(grep -F "  $b" "$sums" | head -1 || true)
  [ -n "$line" ] || die "SHA256SUMS 里找不到 $b"
  want=$(printf '%s' "$line" | awk '{print $1}' | tr 'A-Z' 'a-z')
  if command -v sha256sum >/dev/null; then got=$(sha256sum "$f" | awk '{print $1}')
  elif command -v shasum >/dev/null; then got=$(shasum -a 256 "$f" | awk '{print $1}')
  elif command -v openssl >/dev/null; then got=$(openssl dgst -sha256 "$f" | awk '{print $NF}')
  else die "没有可用的 SHA256 工具"; fi
  [ "$want" = "$got" ] || die "SHA256 校验失败（期望 $want，实际 $got）"
  say "SHA256 OK"; }
unpack() { mkdir -p "$2"; case $EXT in
    zip) if command -v unzip >/dev/null; then unzip -q "$1" -d "$2"; else python3 -c 'import zipfile,sys;zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"; fi ;;
    *)   tar -xzf "$1" -C "$2" ;; esac; }
put_bin() { src=$1; sudo mkdir -p "$INSTALL_DIR"; chmod +x "$src" 2>/dev/null || :
  sudo install -m 0755 "$src" "$INSTALL_DIR/$(basename "$src")" 2>/dev/null || { sudo cp "$src" "$INSTALL_DIR/"; sudo chmod 0755 "$INSTALL_DIR/$(basename "$src")"; }
  BIN_PATH="$INSTALL_DIR/$(basename "$src")"; say "已安装: $BIN_PATH"; }

install_release() {
  detect
  A=${BIN}_${VERSION}_${T}.${EXT}; REL=$REPO/releases/download/$VERSION/$A
  SUMS=${REPO}/releases/download/${VERSION}/SHA256SUMS
  say "下载: $REL"
  dl_one() { fetch "$1/$REL" "$TMP/$A"; }; each dl_list dl_one || die "下载失败 (试 --mirror cn / --doctor)"
  # 校验文件是整版汇总的 SHA256SUMS，不是每个包单独一个 .sha256
  dl_sum() { fetch "$1/$SUMS" "$TMP/SHA256SUMS"; }; each dl_list dl_sum || die "下载 SHA256SUMS 失败"
  sha_verify "$TMP/$A" "$TMP/SHA256SUMS"; unpack "$TMP/$A" "$TMP/x"
  src=$(find "$TMP/x" -type f \( -name "$BIN" -o -name "$BIN.exe" \) | head -1); [ -n "$src" ] || die "解压后找不到二进制"
  put_bin "$src"
}
install_source() {
  command -v cargo >/dev/null || die "--from-source 需要 cargo"; command -v git >/dev/null || die "需要 git"
  say "源码编译 (仓库自带 rsproxy 加速配置) ..."
  cl() { git clone --depth 1 "$1/$REPO.git" "$TMP/src" 2>/dev/null; }; each dl_list cl || die "git clone 失败"
  cargo build --release --manifest-path "$TMP/src/Cargo.toml" || die "cargo build 失败"
  put_bin "$TMP/src/target/release/$BIN"
}

# ---- 配置 / systemd / 卸载 / 自检 ----
new_token() { [ -n "$TOKEN" ] && { printf %s "$TOKEN"; return; }
  if command -v openssl >/dev/null; then openssl rand -hex 24; else od -An -tx1 -N24 /dev/urandom | tr -d ' \n'; fi; }
write_config() {
  TOKEN=$(new_token); [ -n "$TOKEN" ] || die "无法生成密钥，用 --token 指定"
  sudo mkdir -p "$CONFIG_DIR"; ENV=$CONFIG_DIR/matrix-satori.env; STATE=$CONFIG_DIR/data
  if [ -e "$ENV" ]; then say "保留配置: $ENV"; old=$(sed -n 's/^SATORI_TOKEN=//p' "$ENV" | head -1); [ -n "$old" ] && TOKEN=$old
  else cat > "$TMP/env" <<EOF
# matrix-satori 配置 (install.sh 生成，可修改)
MATRIX_HOMESERVER=$HOMESERVER
MATRIX_LOGIN_METHOD=$LOGIN_METHOD
# MATRIX_USERNAME=
# MATRIX_PASSWORD=
# MATRIX_ACCESS_TOKEN=
MATRIX_SSO_CALLBACK_BIND=127.0.0.1:8088
SATORI_BIND=$BIND
SATORI_TOKEN=$TOKEN
MATRIX_STATE_DIR=$STATE
RUST_LOG=info
EOF
    sudo cp "$TMP/env" "$ENV"; sudo chmod 0600 "$ENV"; say "已生成: $ENV"; fi
  sudo mkdir -p "$STATE" 2>/dev/null || :
}
write_systemd() {
  [ "$SYSTEMD" = yes ] || return 0
  command -v systemctl >/dev/null || { say "无 systemctl，跳过 systemd 单元"; return 0; }
  cat > "$TMP/svc" <<EOF
[Unit]
Description=matrix-satori (Matrix -> Satori bridge)
After=network-online.target
[Service]
Type=simple
EnvironmentFile=$ENV
ExecStart=$BIN_PATH
Restart=on-failure
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF
  sudo cp "$TMP/svc" "$CONFIG_DIR/matrix-satori.service"
  if root && [ -d /etc/systemd/system ]; then sudo cp "$TMP/svc" /etc/systemd/system/matrix-satori.service
    systemctl daemon-reload >/dev/null 2>&1 || :; say "已安装 systemd 单元: systemctl enable --now matrix-satori"
  else say "单元模板: $CONFIG_DIR/matrix-satori.service"; fi
}
uninstall() {
  command -v systemctl >/dev/null && systemctl disable --now matrix-satori >/dev/null 2>&1 || :
  sudo rm -f /etc/systemd/system/matrix-satori.service "$INSTALL_DIR/$BIN" "$INSTALL_DIR/$BIN.exe" "$CONFIG_DIR/matrix-satori.service" 2>/dev/null || :
  command -v systemctl >/dev/null && systemctl daemon-reload >/dev/null 2>&1 || :
  say "已卸载 (配置目录 $CONFIG_DIR 保留)"
}
doctor() {
  say "=== matrix-satori 自检 ==="
  for t in curl sh tar; do command -v "$t" >/dev/null && say "  OK  $t" || say "  缺失 $t"; done
  for t in wget unzip python3 sha256sum shasum openssl cargo git; do command -v "$t" >/dev/null && say "  OK  $t" || say "  --  $t (可选)"; done
  detect 2>/dev/null || say "  平台未识别"
  rank; found=0; each api_list latest && found=1
  [ "$found" -eq 1 ] || say "  未找到 Release (用 --from-source)"
  say "诊断完成"
}

main() {
  [ -z "$INSTALL_DIR" ] && { if root; then INSTALL_DIR=/usr/local/bin; else INSTALL_DIR=$HOME/.local/bin; fi; }
  [ -z "$CONFIG_DIR" ] && { if root; then CONFIG_DIR=/etc/matrix-satori; else CONFIG_DIR=${XDG_CONFIG_HOME:-$HOME/.config}/matrix-satori; fi; }
  TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT HUP INT TERM
  if [ "$UNINSTALL" = 1 ]; then uninstall; return 0; fi
  rank
  if [ "$DOCTOR" = 1 ]; then doctor; return 0; fi
  [ "$SYSTEMD" = auto ] && { if root && command -v systemctl >/dev/null; then SYSTEMD=yes; else SYSTEMD=no; fi; }
  if [ "$FROM_SOURCE" = 1 ]; then install_source; else resolve_version; install_release; fi
  write_config; write_systemd
  say ""; say "安装完成!"
  say "  二进制: $BIN_PATH"; say "  配置:   $ENV"; say "  数据:   $STATE"
  say "  Koishi: adapter-satori 的 endpoint 填 $BIND 对应地址，token 填: $TOKEN"
  say "  启动:   $BIN_PATH"
}
main "$@"
