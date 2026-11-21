# matrix-satori

用 **Rust** 写的 [Satori 协议](https://satori.js.org/)服务端，后端对接 **Matrix**，让你把任意 Matrix 账号接到 **Koishi**（最新版 4.18+）里，像用其他适配器一样使用。

```
Koishi (adapter-satori)  ──WebSocket/HTTP──▶  matrix-satori  ──Matrix CS API──▶  任意 homeserver
```

## 特性

- **内置所有主流 Matrix 登录方式**，运行时探测服务器支持的 flows 自动适配，不写死任何 homeserver（详见下方"登录方式"）
- **加密房间（E2EE）支持**：基于官方 `matrix-sdk` 自动解密 olm/megolm，加密房间的消息也能桥接到 Koishi（详见下方"加密房间"）
- 完整实现 Satori 网关：`/v1/events`（READY / IDENTIFY / PING-PONG / EVENT，opcode 0/1/2/3/4/5）
- HTTP API：`message.create/update/delete/get/list`、`channel.*`、`guild.*`、`user.get`、`login.get`、`reaction.*` 等
- Matrix 长轮询 `/sync`，断线自动重连，同步位点 + 凭据落盘（重启不丢消息、不用重复登录）
- Matrix 事件 → Satori 事件：文本、@提及、图片/音频/视频/文件、引用回复、表情回应、成员加入/退出、房间改名、编辑、撤回
- Satori Element ↔ Matrix HTML 双向转换（`<at>`、`<img>`、`<quote>`、`<br>`、`<emoji>`…）
- Matrix 媒体 `mxc://` 自动转换成 HTTP URL
- 令牌鉴权、非 root 容器、多架构 Docker 镜像、GitHub Actions 自动发版

## 登录方式（全支持）

程序启动时会 `GET /_matrix/client/v3/login` 探测服务器支持的登录方式，再按你的配置选择。用 `--login-method`（或 `MATRIX_LOGIN_METHOD`）显式指定，默认 `auto` 自动判断：

| 方式 | `--login-method` | 需要的参数 | 说明 |
| --- | --- | --- | --- |
| 直接 access token | `accesstoken` | `--access-token` | 最快。已有 token 时用，跳过登录 |
| 密码登录 | `password` | `--username` `--password` | `m.login.password`，matrix.org / 自建 Synapse 常用 |
| Token 登录 | `token` | `--login-token` | `m.login.token`，用 SSO 回调产生的 loginToken 换 token |
| SSO 网页登录 | `sso` | （无，交互式） | `m.login.sso`，**程序内置本地回调服务**，打印链接让你浏览器登录，自动换取并保存 token |
| Appservice | `applicationservice` | `--as-token` `--user-id` | `m.login.application_service`，应用服务场景 |

`auto` 的优先级：`--login-token` > 用户名+密码 > appservice > 服务器支持 SSO 则走交互式 SSO > 报错并提示。

登录成功后凭据缓存到 `<state-dir>/credentials.json`（权限 0600，记录 homeserver），下次启动自动复用；换服务器不会误用旧 token。缓存复用规则：只要你没有在命令行/配置里提供新的登录凭据（access token / loginToken / 用户名密码 / as-token），`auto`、`sso`、`password`、`token` 四种方式都会优先读取本地缓存——所以 SSO 登录一次后，后续重启**不用再开浏览器**。直接 `accesstoken` / `applicationservice` 方式自带凭据，不会写入也不会读取缓存。想强制重新登录，删掉 `credentials.json` 即可。

### 加密房间（E2EE）与收发架构

整个程序只有**一条** Matrix 连接，基于官方 Rust SDK（`matrix-sdk`）：登录由本程序自己完成（支持全部方式），拿到的凭据通过 `restore_session` 交给 SDK，之后**收消息和发消息全部走这一条 SDK 连接**——明文房间直接收发，加密房间由 SDK 自动解密/加密。没有第二套 sync，同一条消息只会推一次。

- **收**：SDK 的强类型事件处理器（message/reaction/redaction）统一转成 Satori 事件，加密房间的 `m.room.encrypted` 被透明解密后再投递。
- **发**：`message.create` 等经 `Room::send_raw` 发送，SDK 在加密房间自动加密、明文房间直接发，调用方无需关心房间是否加密。
- **密钥**：通过 SQLite（bundled）持久化到 `<state-dir>/crypto-store`，重启后仍能解密。

**启用条件**：解密要求凭据带 device_id（决定能否恢复该设备的 olm 密钥）：

| 登录方式 | 加密房间 |
| --- | --- |
| 交互式 SSO `sso` | ✅ 支持（回调返回 device_id，直接复用） |
| 密码 `password` | ✅ 支持 |
| Token `token`（SSO 回调的 loginToken） | ✅ 支持 |
| `auto` | ✅ 支持（登录成功即带 device_id） |
| 直接 access token `accesstoken` | ⚠️ 带 `--device-id` 且该设备密钥在本机 store 时可用；否则明文可收发、加密房间解不了 |
| Appservice `applicationservice` | ⚠️ 同上 |

启动日志打印 `[sdk] starting single sync engine (...)` 和 `[sdk] session restored`。

**重要：设备独占**。这个桥接程序会用凭据里的 device_id 恢复会话并上传自己的 one-time keys，因此**该 device 必须专供本程序使用，不要同时在你手机/网页端登录同一个 device**——否则两端会抢密钥（表现为 `One time key ... already exists` 或 `SigningKeyChanged`）。需要手机同时在线就用另一个账号，或给桥接单独建一个 device 登录。state_dir（含 crypto-store 和 credentials.json）要固定并持久化，别每次换目录。

**限制**：
- **历史消息**：Matrix 密钥与设备绑定，本设备创建**之前**发送的加密消息无法解密（需要 cross-signing / 密钥备份恢复，未实现）。从本次登录起、对方重新分发的消息可解。
- 未做跨设备验证（cross-signing / emoji 验证），默认接受未验证设备的消息（能解密，但不做信任校验）。

### 关于 SSO 登录（mozilla.modular.im 这类）

有两条路，任选其一：

**方式 A —— 让程序全权处理（推荐）**

```
--login-method sso
```

程序会在 `--sso-callback-bind`（默认 `127.0.0.1:8088`）起一个临时回调服务，打印形如下面的链接：

```
请在浏览器打开以下链接完成 SSO 登录：

  https://mozilla.modular.im/_matrix/client/v3/login/sso/redirect?redirectUrl=http%3A%2F%2F127.0.0.1%3A8088%2Fsatori-sso-callback
```

浏览器完成登录（Firefox Account / GitHub / Google 等）后，homeserver 会带着 `loginToken` 跳回本地回调，程序自动换取 access token 并缓存，然后继续启动。**注意**：回调地址必须能被你的浏览器访问到；如果程序跑在服务器上，把 `--sso-redirect-uri` 设成公网可达、并指向本机回调的地址（或用 SSH 端口转发）。

**方式 B —— 自己从网页客户端复制 token**

Mozilla 官方给 bot 取 token 的做法（见 [Taskcluster 文档](https://firefox-ci-tc.services.mozilla.com/docs/manual/deploying/matrix)）：

1. 无痕窗口打开 [chat.mozilla.org](https://chat.mozilla.org/)，用 SSO 登录 bot 账号；
2. 头像 → **Settings** → **Help & About** → 拉到底部 **Access Token** 复制；
3. **不要点退出登录**（登出会让 token 失效），直接关掉无痕窗口；
4. 用 `--access-token 'syt_...'` 启动本程序。

> `mozilla.modular.im` 的 `/.well-known/matrix/client` 是 404，务必**显式写 homeserver URL**。`chat.mozilla.org` 只是网页客户端，不是 homeserver API 地址。

## 一键安装（推荐）

自动下载对应平台的预编译二进制、校验 SHA256、生成配置与启动单元。脚本内置** GitHub 加速源回退**，国内网络可用。

> **平台支持**：预编译包只覆盖 Linux（x86_64 / arm64 / armv7）与 Windows（x86_64）。**macOS 不提供预编译包**，请用 `--from-source` 从源码编译（见下方）。

### Linux

单个 CDN 在你的网络里可能被重置（连接被 reset）。用下面这段**多源自动回退**：它挨个试，哪个能拉到脚本就用哪个安装：

```bash
for u in \
 "https://gh-proxy.com/https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh" \
 "https://ghproxy.net/https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh" \
 "https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh" \
 "https://fastly.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.sh" \
 "https://gcore.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.sh" \
 "https://cdn.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.sh"; do
  echo "尝试: $u"
  curl -fsSL --connect-timeout 8 --max-time 25 "$u" -o /tmp/ms-install.sh 2>/dev/null \
    && grep -q 'install_from_release' /tmp/ms-install.sh 2>/dev/null \
    && { echo "成功: $u"; sh /tmp/ms-install.sh; break; } \
    || echo "失败,换下一个"
done
```

网络条件好时，也可以直接用原始地址：

```bash
curl -fsSL https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh | sh
```

### Windows（PowerShell，无需 Git Bash / sh）

```powershell
iwr https://cdn.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

> Windows 原生没有 `sh`，所以单独提供 `install.ps1`。二进制装到 `%LOCALAPPDATA%\matrix-satori\bin`，配置在 `%APPDATA%\matrix-satori\`，并生成一个读取配置后启动的 `start-matrix-satori.ps1`。

安装脚本会：
- 自动识别系统/架构（Linux x86_64 / arm64 / armv7、Windows x86_64），下载对应 Release 包并校验 SHA256；macOS 会提示改用 `--from-source`
- **安装前并发探测各镜像的真实响应延迟，按最快优先选源**（不是按写死顺序 + 超时试错）。API 与下载分别测速、各自排序，因为两者可达性可能不同。探测结果会在开头打印，超时（默认 5 秒）只用于给慢源封顶，不决定优先级
- `--mirror` 决定哪些源参与测速：`auto`（默认，加速源 + 官方直连一起测）、`cn`（只测国内加速）、`direct`（只直连，无需测速）
- 二进制默认装到 `/usr/local/bin`（非 root 装到 `~/.local/bin`；Windows 见上）
- 生成配置模板（随机 Satori token、homeserver、登录方式等）；Linux root + systemd 时另生成服务单元
- **仓库还没发 Release 时**给出明确提示，并可用 `--from-source` 从源码编译（项目内置 rsproxy 镜像）

**国内网络很慢时，可限定只在加速源里测速（推荐）：**

```bash
sh install.sh --mirror cn                       # 只在国内加速源里选最快的，不碰直连
# 或设环境变量（Windows: $env:MATRIX_SATORI_MIRROR="cn"）
MATRIX_SATORI_MIRROR=cn sh install.sh
# 探测/连接都很慢时可缩短等待
sh install.sh --mirror cn --probe-timeout 3 --connect-timeout 5
```

Windows 对应用 `-Mirror cn -ProbeTimeoutSec 3 -TimeoutSec 15`。`--mirror direct` 可强制只走官方源。

装完**先编辑配置**再启动：

```bash
# Linux root 默认 /etc/matrix-satori/matrix-satori.env，非 root 在 ~/.config/matrix-satori/
sudo nano /etc/matrix-satori/matrix-satori.env
# 填入 MATRIX_HOMESERVER、登录凭据，按需改 SATORI_BIND / SATORI_TOKEN
sudo systemctl enable --now matrix-satori
```

### 排障自检

网络报错、装不上时先跑自检（不安装、不改配置，会打印各 API/下载源的实测延迟排名、Release 状态与工具检查）：

```bash
sh install.sh --doctor
```

### 常用参数

```bash
sh install.sh --version v0.1.0 --install-dir ~/.local/bin --no-systemd \
  --homeserver https://matrix.example.com --login-method password
sh install.sh --from-source              # 从源码编译（内置国内 cargo 镜像）
sh install.sh --uninstall                # 卸载
sh install.sh --help                     # 全部选项
```

> 脚本默认自动取**最新 Release**。若某个平台没有预编译包（如 macOS），一键下载会提示改用 `--from-source`。新版本由维护者 `git tag vX.Y.Z` 触发 Release workflow 自动编译发布。SSO 登录需浏览器能访问回调地址（见上方）。容器部署见下方 Docker 章节。

## 手动运行

已经装好二进制、想直接跑：

```bash
matrix-satori \
  --homeserver https://mozilla.modular.im \
  --login-method sso \
  --bind 0.0.0.0:5140 \
  --satori-token '给koishi用的共享密钥'
# 按提示在浏览器完成 SSO 登录，之后服务就绪
```

或者密码登录（换成任意支持密码的服务器即可）：

```bash
matrix-satori \
  --homeserver https://matrix.example.com \
  --login-method password \
  --username mybot --password '你的密码' \
  --satori-token 'shared-secret'
```

也支持全用环境变量：

```bash
export MATRIX_HOMESERVER=https://matrix.example.com
export MATRIX_LOGIN_METHOD=password
export MATRIX_USERNAME=mybot
export MATRIX_PASSWORD='你的密码'
export SATORI_TOKEN='shared-secret'
matrix-satori
```

确认服务正常：

```bash
curl http://127.0.0.1:5140/healthz   # -> ok
```

## Koishi 侧配置

安装最新版 `adapter-satori`（要求 Koishi `^4.18.6`）：

```bash
npm i @koishijs/plugin-adapter-satori
```

```ts
import satori from '@koishijs/plugin-adapter-satori'

ctx.plugin(satori, {
  endpoint: 'http://127.0.0.1:5140',  // matrix-satori 地址
  token: '给koishi用的共享密钥',       // 与 --satori-token 一致
})
```

之后把 bot 邀请进房间（`/invite @your-bot:server`），在房间里发消息，Koishi 就能收到并回应。

## Docker

```bash
docker run -d --name matrix-satori \
  -p 5140:5140 \
  -e MATRIX_HOMESERVER=https://matrix.example.com \
  -e MATRIX_LOGIN_METHOD=password \
  -e MATRIX_USERNAME=mybot \
  -e MATRIX_PASSWORD='你的密码' \
  -e SATORI_TOKEN='shared-secret' \
  -v matrix-satori-data:/app/data \
  ghcr.io/fly1919/matrix-satori:latest
```

SSO 登录在容器里用起来麻烦（回调要能被浏览器访问），**建议先在宿主机用 `sso` 登录一次**，把生成的 `data/credentials.json` 挂载进容器，之后容器会用缓存凭据自动启动。

## 配置参考

**配置文件（TOML）**：首次启动会在 `<state-dir>/config.toml` 自动生成一份带注释的默认配置，编辑后重启生效。也可用 `--config <path>` / `MATRIX_CONFIG` 指定别的位置。优先级：**命令行参数 > 环境变量 > config.toml > 内置默认值**。

```toml
# <state-dir>/config.toml —— 首次启动自动生成
homeserver = "https://matrix.example.com"
login_method = "password"
username = "mybot"
password = "你的密码"
bind = "127.0.0.1:5140"
satori_token = "shared-secret"
sync_timeout = 30000
```

| 参数 | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `--homeserver` | `MATRIX_HOMESERVER` | `http://localhost:8008` | homeserver URL |
| `--login-method` | `MATRIX_LOGIN_METHOD` | `auto` | `auto/password/token/sso/applicationservice/accesstoken` |
| `--access-token` | `MATRIX_ACCESS_TOKEN` | 空 | 已有 access token |
| `--username` | `MATRIX_USERNAME` | 空 | 密码登录用户名 |
| `--password` | `MATRIX_PASSWORD` | 空 | 密码登录密码 |
| `--login-token` | `MATRIX_LOGIN_TOKEN` | 空 | SSO 回调的 loginToken |
| `--device-id` | `MATRIX_DEVICE_ID` | 空 | 登录设备 ID（可选） |
| `--user-id` | `MATRIX_USER_ID` | 自动探测 | bot 的 Matrix ID |
| `--as-token` | `MATRIX_AS_TOKEN` | 空 | appservice token |
| `--sso-callback-bind` | `MATRIX_SSO_CALLBACK_BIND` | `127.0.0.1:8088` | SSO 本地回调监听 |
| `--sso-redirect-uri` | `MATRIX_SSO_REDIRECT_URI` | 自动 | SSO 对外回调 URI |
| `--bind` | `SATORI_BIND` | `127.0.0.1:5140` | Satori 服务监听地址 |
| `--satori-token` | `SATORI_TOKEN` | 空 | 客户端共享密钥，空则不鉴权（公网务必设置） |
| `--path` | `SATORI_PATH` | 空 | 路径前缀，如 `matrix` → `/matrix/v1/events` |
| `--sync-timeout` | `MATRIX_SYNC_TIMEOUT` | `30000` | `/sync` 长轮询超时（毫秒） |
| `--state-dir` | `MATRIX_STATE_DIR` | `./data` | 凭据与同步位点目录 |
| `--log` / `RUST_LOG` | `RUST_LOG` | `info` | 日志级别 |
| `--config` | `MATRIX_CONFIG` | `<state-dir>/config.toml` | 配置文件路径 |

## 已验证 / 限制

- **46 个测试全部通过**：单元（元素解析 / 登录名 / TOML 配置自动创建·读取·CLI 覆盖 / E2EE 可用性判断）+ 9 登录方式集成（password / token / appservice / access-token / auto / flows 探测 / 凭据缓存）+ 6 服务端集成（WS READY、事件推送、PING-PONG、message.create 调用链、鉴权、reaction / channel / upload API、全方法非 404）。登录测试用 mock homeserver 验证真实 HTTP 交互。
- **单一 SDK 收发 + E2EE 已支持**：基于官方 `matrix-sdk` 用 `restore_session` 复用登录设备，全局只有一条连接，收消息和发消息都走它（加密房间自动加/解密，同一条消息只推一次）；SSO / password / token / auto 均可用，密钥经 SQLite 持久化、重启后仍可解密。启用条件、设备独占要求与历史消息限制见上方「加密房间（E2EE）与收发架构」章节。
- 反应删除（`reaction.delete/clear`）需要 Matrix 侧的反应事件 ID，而 Satori 该 action 不带，目前是空操作。

## 开发

```bash
cargo build --release
cargo test              # 18 + 7 + 3 = 28
cargo clippy --all-targets
cargo fmt
```

## License

MIT
