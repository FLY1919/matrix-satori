# matrix-satori

用 **Rust** 写的 [Satori 协议](https://satori.js.org/)服务端，后端对接 **Matrix**，让你把任意 Matrix 账号接到 **Koishi**（最新版 4.18+）里，像用其他适配器一样使用。

```
Koishi (adapter-satori)  ──WebSocket/HTTP──▶  matrix-satori  ──Matrix CS API──▶  任意 homeserver
```

## 特性

- **内置所有主流 Matrix 登录方式**，运行时探测服务器支持的 flows 自动适配，不写死任何 homeserver（详见下方"登录方式"）
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

登录成功后凭据缓存到 `<state-dir>/credentials.json`（记录 homeserver），下次启动自动复用；换服务器不会误用旧 token。

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

一行命令自动下载对应平台的预编译二进制、校验 SHA256、生成配置与 systemd 单元：

```bash
curl -fsSL https://raw.githubusercontent.com/FLY1919/matrix-satori/main/install.sh | sh
```

安装脚本会：
- 自动识别系统/架构（Linux x86_64 / arm64 / armv7、macOS Intel / Apple Silicon、Windows x86_64），从最新 Release 下载对应包并校验 SHA256
- 二进制默认装到 `/usr/local/bin`（非 root 装到 `~/.local/bin`）
- 生成 `matrix-satori.env` 配置模板（随机 Satori token、homeserver、登录方式等）和 systemd 单元
- root 且检测到 systemd 时，提示用 `systemctl enable --now matrix-satori` 启动

装完**先编辑配置**再启动：

```bash
# root 默认在 /etc/matrix-satori/matrix-satori.env，非 root 在 ~/.config/matrix-satori/
sudo nano /etc/matrix-satori/matrix-satori.env
# 填入 MATRIX_HOMESERVER、登录凭据，按需改 SATORI_BIND / SATORI_TOKEN
sudo systemctl enable --now matrix-satori
```

常用参数：

```bash
# 指定版本 / 安装目录 / 不开 systemd / 自定义 homeserver 与登录方式
sh install.sh --version v0.1.0 --install-dir ~/.local/bin --no-systemd \
  --homeserver https://matrix.example.com --login-method password

sh install.sh --from-source                     # 本机有 Rust 时从源码编译
sh install.sh --uninstall                       # 卸载
sh install.sh --help                            # 全部选项
```

> SSO 登录需要浏览器能访问回调地址，跑在服务器上时要注意（见上方"关于 SSO 登录"）。容器部署见下方 Docker 章节。

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

## 已验证 / 限制

- **28 个测试全部通过**：18 单元 + 7 登录方式集成（password / token / appservice / access-token / auto / flows 探测 / 凭据缓存）+ 3 服务端集成（WS READY、事件推送、PING-PONG、message.create 调用链、鉴权）。登录测试用 mock homeserver 验证真实 HTTP 交互。
- **端到端加密（E2EE）暂不支持**：加密房间的 `m.room.encrypted` 事件会被忽略并打印警告。要支持需集成 olm/machine 解密（复杂度和体积大幅上升）。
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
