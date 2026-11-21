<#
.SYNOPSIS
    matrix-satori 一键安装（Windows 原生 PowerShell，无需 Git Bash / sh）

.DESCRIPTION
    从 GitHub Releases 下载 Windows 预编译包，校验 SHA256，解压安装，
    生成配置模板和一个读取配置后启动的脚本。支持源码编译回退与卸载。

.EXAMPLE
    iwr https://cdn.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.ps1 -OutFile install.ps1
    powershell -ExecutionPolicy Bypass -File .\install.ps1

.EXAMPLE
    .\install.ps1 -Version v0.1.0 -InstallDir C:\Tools\matrix-satori -LoginMethod password
    .\install.ps1 -FromSource
    .\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$Version      = "",
    [string]$InstallDir   = "",
    [string]$ConfigDir    = "",
    [string]$Homeserver   = "https://mozilla.modular.im",
    [string]$LoginMethod  = "sso",
    [string]$Bind         = "127.0.0.1:5140",
    [string]$Token        = "",
    [string]$Repo         = "FLY1919/matrix-satori",
    [string]$ApiBase      = "https://api.github.com",
    [string]$DownloadBase = "https://github.com",
    [ValidateSet("auto", "cn", "direct")]
    [string]$Mirror       = "auto",     # auto=加速+直连一起测速 / cn=只测国内加速 / direct=只直连
    [int]$TimeoutSec      = 20,         # 单次下载请求超时
    [int]$ProbeTimeoutSec = 5,          # 测速探测上限；超时只给探测封顶，不决定优先级
    [string]$Target       = "",
    [switch]$FromSource,
    [switch]$Uninstall,
    [switch]$NoPath,
    [switch]$Help
)

$ErrorActionPreference = "Stop"
$Bin = "matrix-satori"
# 环境变量可预设镜像模式
if ($env:MATRIX_SATORI_MIRROR) { $Mirror = $env:MATRIX_SATORI_MIRROR }

function Info { param([string]$m) Write-Host $m -ForegroundColor Green }
function Warn { param([string]$m) Write-Host $m -ForegroundColor Yellow }
function Fail { param([string]$m) Write-Host "错误: $m" -ForegroundColor Red; exit 1 }

# ---- 镜像候选：返回 [pscustomobject] 列表 (Name / Api / Download) ----
# 这里只是候选，不代表优先级；实际优先级由并发测速决定（谁快用谁）。
function Get-Mirrors {
    $cn = @(
        [pscustomobject]@{ Name = "gh-proxy";  Api = "https://gh-proxy.com/https://api.github.com";  Download = "https://gh-proxy.com/https://github.com" },
        [pscustomobject]@{ Name = "ghproxy.net"; Api = "https://ghproxy.net/https://api.github.com"; Download = "https://ghproxy.net/https://github.com" },
        [pscustomobject]@{ Name = "ghfast.top"; Api = ""; Download = "https://ghfast.top/https://github.com" },
        [pscustomobject]@{ Name = "gitmirror"; Api = "https://hub.gitmirror.com/https://api.github.com"; Download = "https://hub.gitmirror.com/https://github.com" }
    )
    $direct = @(
        [pscustomobject]@{ Name = "github"; Api = "https://api.github.com"; Download = "https://github.com" }
    )
    switch ($Mirror) {
        "cn"     { return $cn }
        "direct" { return $direct }
        default  { return @($cn) + @($direct) }   # auto: 两类源都参与测速
    }
}

# ---- 并发测速选源 ----
# API 与下载走不同 URL 路径，速度和可达性可能不同，因此分开测速、各自排序。
# 用 HttpWebRequest 对真实用途 URL 发 GET，Stopwatch 计时；超时只封顶，不决定优先级。
$script:RankedApi = $null
$script:RankedDownload = $null
$script:ProbeReport = $null

# 探测单个 URL 的耗时（秒），失败/超时返回 999999。定义成脚本块以便 ThreadJob 内联复用。
$ProbeScript = {
    param([string]$Url, [int]$ToSec)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $req = [System.Net.HttpWebRequest]::Create($Url)
        $req.Method = "GET"
        $req.Timeout = $ToSec * 1000
        $req.ReadWriteTimeout = $ToSec * 1000
        $req.Headers["User-Agent"] = "matrix-satori-installer"
        $resp = $req.GetResponse()   # 只探测连通与耗时，不读响应体
        $resp.Close()
        $sw.Stop()
        return $sw.Elapsed.TotalSeconds
    } catch {
        $sw.Stop()
        return 999999.0
    }
}
function Invoke-Probe { param([string]$Url) return (& $ProbeScript -Url $Url -ToSec $ProbeTimeoutSec) }

function Invoke-Ranking {
    if ($null -ne $script:RankedApi -and $null -ne $script:RankedDownload) { return }
    $mirrors = @(Get-Mirrors)
    Info "并发探测镜像延迟（超时上限 ${ProbeTimeoutSec}s，API/下载分别测速，快→慢）..."

    # 优先用 ThreadJob 并发；不可用时退化为顺序测速（结果一致，只是慢一些）
    $hasThreadJob = [bool](Get-Command Start-ThreadJob -ErrorAction SilentlyContinue)
    $apiJobs = @(); $dlJobs = @(); $apiMeta = @(); $dlMeta = @()

    foreach ($m in $mirrors) {
        if ($m.Api) {
            $url = "$($m.Api)/repos/$Repo/releases/latest"
            $apiMeta += [pscustomobject]@{ Name = $m.Name; Url = $url }
            if ($hasThreadJob) { $apiJobs += Start-ThreadJob -ScriptBlock $ProbeScript -ArgumentList $url, $ProbeTimeoutSec }
        }
        if ($m.Download) {
            $url = "$($m.Download)/$Repo"
            $dlMeta += [pscustomobject]@{ Name = $m.Name; Url = $url }
            if ($hasThreadJob) { $dlJobs += Start-ThreadJob -ScriptBlock $ProbeScript -ArgumentList $url, $ProbeTimeoutSec }
        }
    }

    if ($hasThreadJob) {
        $apiLat = @($apiJobs | ForEach-Object { $r = Receive-Job -Job $_ -Wait; Remove-Job -Job $_; $r })
        $dlLat  = @($dlJobs  | ForEach-Object { $r = Receive-Job -Job $_ -Wait; Remove-Job -Job $_; $r })
    } else {
        $apiLat = @(); foreach ($x in $apiMeta) { $apiLat += Invoke-Probe -Url $x.Url }
        $dlLat  = @(); foreach ($x in $dlMeta)  { $dlLat  += Invoke-Probe -Url $x.Url }
    }

    $apiRes = @(); for ($i = 0; $i -lt $apiMeta.Count; $i++) { $apiRes += [pscustomobject]@{ Url = $apiMeta[$i].Url; Lat = $apiLat[$i] } }
    $dlRes  = @(); for ($i = 0; $i -lt $dlMeta.Count;  $i++) { $dlRes  += [pscustomobject]@{ Url = $dlMeta[$i].Url;  Lat = $dlLat[$i]  } }

    $script:RankedApi = @($apiRes | Sort-Object Lat | ForEach-Object { $_.Url })
    $script:RankedDownload = @($dlRes | Sort-Object Lat | ForEach-Object { $_.Url })

    Info "  API 源:"
    foreach ($r in ($apiRes | Sort-Object Lat)) {
        if ($r.Lat -ge 999998) { Info "    - 超时  $($r.Url)" } else { Info "    - $([math]::Round($r.Lat,3))s  $($r.Url)" }
    }
    Info "  下载源:"
    foreach ($r in ($dlRes | Sort-Object Lat)) {
        if ($r.Lat -ge 999998) { Info "    - 超时  $($r.Url)" } else { Info "    - $([math]::Round($r.Lat,3))s  $($r.Url)" }
    }
    $script:ProbeReport = [pscustomobject]@{ Api = $apiRes; Download = $dlRes }
}

# 用户自定义基址时优先使用，其余按测速排名（谁快用谁）
function Get-ApiSources {
    Invoke-Ranking
    $list = @()
    if ($ApiBase -ne "https://api.github.com") { $list += $ApiBase }
    foreach ($u in $script:RankedApi) { $list += $u }
    return $list
}
function Get-DownloadSources {
    Invoke-Ranking
    $list = @()
    if ($DownloadBase -ne "https://github.com") { $list += $DownloadBase }
    foreach ($u in $script:RankedDownload) { $list += $u }
    return $list
}

if ($Help) {
    Get-Help $PSCommandPath -Full
    exit 0
}

# ---- 目录默认值 ----
if (-not $InstallDir) {
    if ($IsWindows -and $env:LOCALAPPDATA) { $InstallDir = Join-Path $env:LOCALAPPDATA "matrix-satori\bin" }
    else { $InstallDir = Join-Path $HOME ".local\bin" }
}
if (-not $ConfigDir) {
    if ($IsWindows -and $env:APPDATA) { $ConfigDir = Join-Path $env:APPDATA "matrix-satori" }
    else { $ConfigDir = Join-Path $HOME ".config\matrix-satori" }
}

# ---- 目标平台 ----
function Get-Target {
    if ($Target) { return $Target }
    if ($IsWindows) {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        switch ($arch) {
            "X64"   { return "x86_64-pc-windows-msvc" }
            "Arm64" { Fail "Windows ARM64 暂无预编译包，请用 -FromSource 源码编译" }
            default { Fail "不支持的架构: $arch" }
        }
    }
    Fail "install.ps1 面向 Windows；非 Windows 请用 install.sh（或显式传 -Target 仅用于测试）"
}

# ---- 版本解析 ----
function Resolve-Version {
    if ($Version) { return $Version }
    Info "正在查询最新 Release ...（镜像模式: $Mirror）"
    $tag = ""
    foreach ($base in (Get-ApiSources)) {
        Info "  查询 API: $base"
        try {
            $meta = Invoke-RestMethod -Uri "$base/repos/$Repo/releases/latest" `
                -Headers @{ "User-Agent" = "matrix-satori-installer" } -TimeoutSec $TimeoutSec
            if ($meta.tag_name) { $tag = $meta.tag_name; Info "  ✓ 命中: $base"; break }
        } catch {
            Warn "  ✗ 失败: $base ($($_.Exception.Message))"
        }
    }
    if (-not $tag) {
        Fail "查询不到最新 Release：可能是仓库未发版或网络不可达。可试 -Mirror cn、-Version v0.1.0，或用 -FromSource 编译。"
    }
    Info "最新版本: $tag"
    return $tag
}

# ---- 随机密钥 ----
function New-Token {
    $bytes = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(24)
    return -join ($bytes | ForEach-Object { $_.ToString("x2") })
}

# ---- 校验 SHA256 ----
function Verify-Checksum {
    param([string]$File, [string]$SumFile)
    $line = (Get-Content $SumFile -First 1)
    if (-not $line) { Fail "校验文件为空" }
    $expected = ($line -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $File).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { Fail "SHA256 校验失败（期望 $expected，实际 $actual），已中止安装" }
    Info "SHA256 校验通过"
}

# ---- 从 Release 安装 ----
function Download-Asset {
    param([string]$Rel, [string]$Out)
    foreach ($base in (Get-DownloadSources)) {
        Info "  尝试下载源: $base"
        try {
            Invoke-WebRequest -Uri "$base/$Rel" -OutFile $Out -UseBasicParsing -TimeoutSec $TimeoutSec
            return $true
        } catch {
            Remove-Item -Path $Out -Force -ErrorAction SilentlyContinue
            Warn "  ✗ 失败: $base"
        }
    }
    return $false
}

function Install-FromRelease {
    $t = Get-Target
    $asset = "${Bin}_${Version}_${t}.zip"
    $rel = "$Repo/releases/download/$Version/$asset"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ms-install-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null

    Info "目标平台: $t"
    Info "下载资产: $rel （镜像模式: $Mirror）"
    $zip = Join-Path $tmp $asset
    $sum = "$zip.sha256"
    if (-not (Download-Asset -Rel $rel -Out $zip)) {
        Fail "下载失败（已尝试全部镜像源）。可试 -Mirror cn、检查代理，或用 -FromSource 编译。"
    }
    if (-not (Download-Asset -Rel "$rel.sha256" -Out $sum)) {
        Fail "下载 .sha256 失败（已尝试全部镜像源）。"
    }
    Verify-Checksum -File $zip -SumFile $sum

    Info "解压 ..."
    $ex = Join-Path $tmp "extract"
    try { Expand-Archive -Path $zip -DestinationPath $ex -Force } catch { Fail "解压失败: $($_.Exception.Message)" }
    $exe = Get-ChildItem -Path $ex -Recurse -Filter "$Bin.exe" | Select-Object -First 1
    if (-not $exe) { Fail "解压后找不到 $Bin.exe" }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir "$Bin.exe"
    Copy-Item -Path $exe.FullName -Destination $dest -Force
    $script:BinPath = $dest
    Info "已安装: $dest"

    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# ---- 源码编译 ----
function Install-FromSource {
    $git = Get-Command git -ErrorAction SilentlyContinue
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $git -or -not $cargo) { Fail "-FromSource 需要本机安装 git 与 Rust(cargo)" }
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ms-src-" + [guid]::NewGuid().ToString("N"))
    Info "从源码编译（可能需要几分钟）..."
    $cloned = $false
    foreach ($base in (Get-DownloadSources)) {
        Info "  git clone via $base"
        & git clone --depth 1 "$base/$Repo.git" $tmp 2>$null
        if ($LASTEXITCODE -eq 0) { $cloned = $true; break }
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (-not $cloned) { Fail "git clone 失败（已尝试全部镜像源）" }
    & cargo build --release --manifest-path (Join-Path $tmp "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { Fail "cargo build 失败" }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir "$Bin.exe"
    $built = Join-Path $tmp "target\release\$Bin.exe"
    if (-not (Test-Path $built)) { $built = Join-Path $tmp "target\release\$Bin" }
    Copy-Item -Path $built -Destination $dest -Force
    $script:BinPath = $dest
    Info "已安装: $dest"
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

# ---- 配置生成 ----
function Write-Config {
    if (-not $Token) { $Token = New-Token }
    if (-not $Token) { Fail "无法生成随机密钥，请用 -Token 指定" }
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
    $envFile = Join-Path $ConfigDir "config.env"
    $stateDir = Join-Path $ConfigDir "data"

    if (Test-Path $envFile) {
        Info "保留已有配置: $envFile"
        $old = (Select-String -Path $envFile -Pattern '^SATORI_TOKEN=(.*)$' | Select-Object -First 1)
        if ($old) { $script:Token = $old.Matches[0].Groups[1].Value }
    } else {
        $content = @(
            "# matrix-satori 配置（由 install.ps1 生成，可按需修改）"
            "MATRIX_HOMESERVER=$Homeserver"
            "MATRIX_LOGIN_METHOD=$LoginMethod"
            "# 密码登录时填写下面两项:"
            "# MATRIX_USERNAME="
            "# MATRIX_PASSWORD="
            "# 已有 access token 登录时填写:"
            "# MATRIX_ACCESS_TOKEN="
            "MATRIX_SSO_CALLBACK_BIND=127.0.0.1:8088"
            "SATORI_BIND=$Bind"
            "SATORI_TOKEN=$Token"
            "MATRIX_STATE_DIR=$stateDir"
            "RUST_LOG=info"
        )
        Set-Content -Path $envFile -Value $content -Encoding UTF8
        Info "已生成配置: $envFile"
    }
    New-Item -ItemType Directory -Force -Path $stateDir -ErrorAction SilentlyContinue | Out-Null

    # 生成读取配置并启动的脚本
    $start = Join-Path $ConfigDir "start-matrix-satori.ps1"
    $placeholder = "__BINPATH__"
    $startContent = @'
# 读取 config.env  setting 环境变量后启动 matrix-satori
$envFile = Join-Path $PSScriptRoot 'config.env'
if (Test-Path $envFile) {
    Get-Content $envFile | Where-Object { $_ -and -not $_.StartsWith('#') -and $_ -match '=' } | ForEach-Object {
        $idx = $_.IndexOf('=')
        $k = $_.Substring(0, $idx).Trim()
        $v = $_.Substring($idx + 1).Trim()
        Set-Item -Path "Env:$k" -Value $v
    }
}
$exe = '__BINPATH__'
if (-not (Test-Path $exe)) { Write-Host "找不到 $exe" -ForegroundColor Red; exit 1 }
& $exe
'@ -replace $placeholder, ($BinPath -replace '\\', '\\')
    Set-Content -Path $start -Value $startContent -Encoding UTF8
    Info "已生成启动脚本: $start"
}

# ---- 加入 PATH ----
function Add-ToPath {
    if (-not $IsWindows -or $NoPath) { return }
    try {
        $p = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($p -notlike "*$InstallDir*") {
            [Environment]::SetEnvironmentVariable("Path", "$p;$InstallDir", "User")
            Info "已把 $InstallDir 加入用户 PATH（重开终端生效）"
        }
    } catch { Warn "无法自动加入 PATH，请手动添加 $InstallDir: $($_.Exception.Message)" }
}

# ---- 卸载 ----
function Do-Uninstall {
    Remove-Item -Path (Join-Path $InstallDir "$Bin.exe") -Force -ErrorAction SilentlyContinue
    Remove-Item -Path (Join-Path $ConfigDir "start-matrix-satori.ps1") -Force -ErrorAction SilentlyContinue
    if ($IsWindows) {
        try {
            $p = [Environment]::GetEnvironmentVariable("Path", "User")
            if ($p -like "*$InstallDir*") {
                $p2 = ($p -split ';' | Where-Object { $_ -and $_ -ne $InstallDir }) -join ';'
                [Environment]::SetEnvironmentVariable("Path", $p2, "User")
            }
        } catch {}
    }
    Info "已卸载 $InstallDir\$Bin.exe 与启动脚本"
    Info "配置与数据目录 $ConfigDir 已保留（如需删除请手动 Remove-Item）"
}

# ---- 主流程 ----
if ($Uninstall) { Do-Uninstall; exit 0 }

if ($FromSource) { Install-FromSource }
else { $Version = Resolve-Version; Install-FromRelease }

Write-Config
Add-ToPath

Write-Host ""
Info "安装完成!"
Info "  二进制:   $BinPath"
Info "  配置:     $(Join-Path $ConfigDir 'config.env')"
Info "  数据目录: $(Join-Path $ConfigDir 'data')"
Info "  Koishi:   adapter-satori 的 endpoint 填 $Bind 对应地址，token 填: $Token"
Info "  启动:     powershell -ExecutionPolicy Bypass -File `"$(Join-Path $ConfigDir 'start-matrix-satori.ps1')`""
