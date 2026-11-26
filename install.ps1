<#
.SYNOPSIS
    matrix-satori 一键安装（Windows 原生 PowerShell，无需 Git Bash / sh）

.EXAMPLE
    iwr https://cdn.jsdelivr.net/gh/FLY1919/matrix-satori@main/install.ps1 -OutFile install.ps1
    powershell -ExecutionPolicy Bypass -File .\install.ps1

.EXAMPLE
    .\install.ps1 -Version v0.2.10 -InstallDir C:\Tools -LoginMethod password
    .\install.ps1 -FromSource
    .\install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$Version = "", [string]$InstallDir = "", [string]$ConfigDir = "",
    [string]$Homeserver = "https://mozilla.modular.im", [string]$LoginMethod = "sso",
    [string]$Bind = "127.0.0.1:5140", [string]$Token = "", [string]$Repo = "FLY1919/matrix-satori",
    [string]$ApiBase = "https://api.github.com", [string]$DownloadBase = "https://github.com",
    [ValidateSet("auto", "cn", "direct")][string]$Mirror = "auto",
    [int]$TimeoutSec = 20, [int]$ProbeTimeoutSec = 5, [string]$Target = "",
    [switch]$FromSource, [switch]$Uninstall, [switch]$NoPath, [switch]$Help
)
$ErrorActionPreference = "Stop"
$Bin = "matrix-satori"
if ($env:MATRIX_SATORI_MIRROR) { $Mirror = $env:MATRIX_SATORI_MIRROR }

function Info { param($m) Write-Host $m -ForegroundColor Green }
function Warn { param($m) Write-Host $m -ForegroundColor Yellow }
function Fail { param($m) Write-Host "错误: $m" -ForegroundColor Red; exit 1 }

# ---- 镜像候选 (Name/Api/Download)，优先级由下面的并发测速决定 ----
function Get-Mirrors {
    $cn = @(
        [pscustomobject]@{ Name = "gh-proxy"; Api = "https://gh-proxy.com/https://api.github.com"; Download = "https://gh-proxy.com/https://github.com" },
        [pscustomobject]@{ Name = "ghproxy.net"; Api = "https://ghproxy.net/https://api.github.com"; Download = "https://ghproxy.net/https://github.com" },
        [pscustomobject]@{ Name = "gitmirror"; Api = "https://hub.gitmirror.com/https://api.github.com"; Download = "https://hub.gitmirror.com/https://github.com" }
    )
    $direct = @([pscustomobject]@{ Name = "github"; Api = "https://api.github.com"; Download = "https://github.com" })
    switch ($Mirror) { "cn" { return $cn } "direct" { return $direct } default { return @($cn) + @($direct) } }
}

# ---- 并发测速：API 与下载分开排名，谁快用谁 ----
$script:RankedApi = $null; $script:RankedDownload = $null
$ProbeScript = {
    param([string]$Url, [int]$ToSec)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $req = [System.Net.HttpWebRequest]::Create($Url)
        $req.Method = "GET"; $req.Timeout = $ToSec * 1000; $req.ReadWriteTimeout = $ToSec * 1000
        $req.Headers["User-Agent"] = "matrix-satori-installer"
        $resp = $req.GetResponse(); $resp.Close(); $sw.Stop(); return $sw.Elapsed.TotalSeconds
    } catch { $sw.Stop(); return 999999.0 }
}
function Invoke-Ranking {
    if ($null -ne $script:RankedApi -and $null -ne $script:RankedDownload) { return }
    Info "并发探测镜像延迟（上限 ${ProbeTimeoutSec}s，API/下载分别测速，快→慢）..."
    $mirrors = @(Get-Mirrors)
    $hasThreadJob = [bool](Get-Command Start-ThreadJob -ErrorAction SilentlyContinue)
    $apiMeta = @($mirrors | Where-Object { $_.Api } | ForEach-Object { "$($_.Api)/repos/$Repo/releases/latest" })
    $dlMeta  = @($mirrors | Where-Object { $_.Download } | ForEach-Object { "$($_.Download)/$Repo" })
    if ($hasThreadJob) {
        $apiJobs = @($apiMeta | ForEach-Object { Start-ThreadJob -ScriptBlock $ProbeScript -ArgumentList $_, $ProbeTimeoutSec })
        $dlJobs  = @($dlMeta  | ForEach-Object { Start-ThreadJob -ScriptBlock $ProbeScript -ArgumentList $_, $ProbeTimeoutSec })
        $apiLat = @($apiJobs | ForEach-Object { $r = Receive-Job -Job $_ -Wait; Remove-Job -Job $_; $r })
        $dlLat  = @($dlJobs  | ForEach-Object { $r = Receive-Job -Job $_ -Wait; Remove-Job -Job $_; $r })
    } else {
        $apiLat = @($apiMeta | ForEach-Object { & $ProbeScript -Url $_ -ToSec $ProbeTimeoutSec })
        $dlLat  = @($dlMeta  | ForEach-Object { & $ProbeScript -Url $_ -ToSec $ProbeTimeoutSec })
    }
    $apiRes = @(); for ($i = 0; $i -lt $apiMeta.Count; $i++) { $apiRes += [pscustomobject]@{ Url = $apiMeta[$i]; Lat = $apiLat[$i] } }
    $dlRes  = @(); for ($i = 0; $i -lt $dlMeta.Count;  $i++) { $dlRes  += [pscustomobject]@{ Url = $dlMeta[$i];  Lat = $dlLat[$i]  } }
    $script:RankedApi = @($apiRes | Sort-Object Lat | ForEach-Object { $_.Url })
    $script:RankedDownload = @($dlRes | Sort-Object Lat | ForEach-Object { $_.Url })
    foreach ($r in ($apiRes | Sort-Object Lat)) { if ($r.Lat -ge 999998) { Info "  超时  $($r.Url)" } else { Info "  $([math]::Round($r.Lat,3))s  $($r.Url)" } }
}
function Get-ApiSources { Invoke-Ranking; $list = @(); if ($ApiBase -ne "https://api.github.com") { $list += $ApiBase }; return @($list) + @($script:RankedApi) }
function Get-DownloadSources { Invoke-Ranking; $list = @(); if ($DownloadBase -ne "https://github.com") { $list += $DownloadBase }; return @($list) + @($script:RankedDownload) }

if ($Help) { Get-Help $PSCommandPath -Full; exit 0 }

# ---- 目录默认值 ----
if (-not $InstallDir) {
    if ($IsWindows -and $env:LOCALAPPDATA) { $InstallDir = Join-Path $env:LOCALAPPDATA "matrix-satori\bin" } else { $InstallDir = Join-Path $HOME ".local\bin" }
}
if (-not $ConfigDir) {
    if ($IsWindows -and $env:APPDATA) { $ConfigDir = Join-Path $env:APPDATA "matrix-satori" } else { $ConfigDir = Join-Path $HOME ".config\matrix-satori" }
}

function Get-Target {
    if ($Target) { return $Target }
    if (-not $IsWindows) { Fail "install.ps1 面向 Windows；非 Windows 请用 install.sh" }
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    switch ($arch) {
        "X64" { return "x86_64-pc-windows-msvc" }
        "Arm64" { Fail "Windows ARM64 暂无预编译包，请用 -FromSource" }
        default { Fail "不支持的架构: $arch" }
    }
}
function Resolve-Version {
    if ($Version) { return $Version }
    Info "查询最新 Release ...（模式: $Mirror）"
    foreach ($base in (Get-ApiSources)) {
        try {
            $meta = Invoke-RestMethod -Uri "$base/repos/$Repo/releases/latest" -Headers @{ "User-Agent" = "matrix-satori-installer" } -TimeoutSec $TimeoutSec
            if ($meta.tag_name) { Info "最新版: $($meta.tag_name)"; return $meta.tag_name }
        } catch { Warn "  ✗ $base" }
    }
    Fail "查不到 Release: 用 -Version 指定 / -Mirror cn / -FromSource 编译"
}
function New-Token { $b = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(24); return -join ($b | ForEach-Object { $_.ToString("x2") }) }
function Verify-Checksum {
    param($File, $SumFile)
    $name = Split-Path $File -Leaf
    $line = Get-Content $SumFile | Where-Object { $_ -match [regex]::Escape($name) + '\s*\r?$' } | Select-Object -First 1
    if (-not $line) { Fail "SHA256SUMS 里找不到 $name" }
    $expected = ($line -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -Path $File).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { Fail "SHA256 校验失败（期望 $expected，实际 $actual）" }
    Info "SHA256 校验通过"
}
function Download-Asset {
    param($Rel, $Out)
    foreach ($base in (Get-DownloadSources)) {
        try { Invoke-WebRequest -Uri "$base/$Rel" -OutFile $Out -UseBasicParsing -TimeoutSec $TimeoutSec; return $true }
        catch { Remove-Item -Path $Out -Force -ErrorAction SilentlyContinue; Warn "  ✗ $base" }
    }
    return $false
}
function Install-Binary {
    param($Src)
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir "$Bin.exe"; Copy-Item -Path $Src -Destination $dest -Force
    $script:BinPath = $dest; Info "已安装: $dest"
}
function Install-FromRelease {
    $t = Get-Target; $asset = "${Bin}_${Version}_${t}.zip"; $rel = "$Repo/releases/download/$Version/$asset"
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ms-install-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    Info "目标平台: $t`n下载: $rel"
    $zip = Join-Path $tmp $asset; $sum = Join-Path $tmp "SHA256SUMS"
    $sumsRel = "$Repo/releases/download/$Version/SHA256SUMS"
    if (-not (Download-Asset -Rel $rel -Out $zip)) { Fail "下载失败（已试全部源），试 -Mirror cn / -FromSource" }
    if (-not (Download-Asset -Rel $sumsRel -Out $sum)) { Fail "下载 SHA256SUMS 失败" }
    Verify-Checksum -File $zip -SumFile $sum
    $ex = Join-Path $tmp "extract"
    try { Expand-Archive -Path $zip -DestinationPath $ex -Force } catch { Fail "解压失败: $($_.Exception.Message)" }
    $exe = Get-ChildItem -Path $ex -Recurse -Filter "$Bin.exe" | Select-Object -First 1
    if (-not $exe) { Fail "解压后找不到 $Bin.exe" }
    Install-Binary -Src $exe.FullName
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
function Install-FromSource {
    if (-not (Get-Command git -ErrorAction SilentlyContinue) -or -not (Get-Command cargo -ErrorAction SilentlyContinue)) { Fail "-FromSource 需要 git 与 cargo" }
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("ms-src-" + [guid]::NewGuid().ToString("N"))
    Info "源码编译（仓库自带 rsproxy 加速）..."
    $cloned = $false
    foreach ($base in (Get-DownloadSources)) {
        & git clone --depth 1 "$base/$Repo.git" $tmp 2>$null
        if ($LASTEXITCODE -eq 0) { $cloned = $true; break }
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (-not $cloned) { Fail "git clone 失败" }
    & cargo build --release --manifest-path (Join-Path $tmp "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { Fail "cargo build 失败" }
    $built = Join-Path $tmp "target\release\$Bin.exe"
    if (-not (Test-Path $built)) { $built = Join-Path $tmp "target\release\$Bin" }
    Install-Binary -Src $built
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
function Write-Config {
    if (-not $script:Token) { $script:Token = New-Token }
    if (-not $script:Token) { Fail "无法生成密钥，用 -Token 指定" }
    New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
    $envFile = Join-Path $ConfigDir "config.env"; $stateDir = Join-Path $ConfigDir "data"
    if (Test-Path $envFile) {
        Info "保留已有配置: $envFile"
        $old = Select-String -Path $envFile -Pattern '^SATORI_TOKEN=(.*)$' | Select-Object -First 1
        if ($old) { $script:Token = $old.Matches[0].Groups[1].Value }
    } else {
        $content = @(
            "# matrix-satori 配置（install.ps1 生成，可修改）",
            "MATRIX_HOMESERVER=$Homeserver", "MATRIX_LOGIN_METHOD=$LoginMethod",
            "# MATRIX_USERNAME=", "# MATRIX_PASSWORD=", "# MATRIX_ACCESS_TOKEN=",
            "MATRIX_SSO_CALLBACK_BIND=127.0.0.1:8088", "SATORI_BIND=$Bind",
            "SATORI_TOKEN=$Token", "MATRIX_STATE_DIR=$stateDir", "RUST_LOG=info"
        )
        Set-Content -Path $envFile -Value $content -Encoding UTF8; Info "已生成: $envFile"
    }
    New-Item -ItemType Directory -Force -Path $stateDir -ErrorAction SilentlyContinue | Out-Null
    $start = Join-Path $ConfigDir "start-matrix-satori.ps1"
    $startContent = @'
$envFile = Join-Path $PSScriptRoot 'config.env'
if (Test-Path $envFile) {
    Get-Content $envFile | Where-Object { $_ -and -not $_.StartsWith('#') -and $_ -match '=' } | ForEach-Object {
        $i = $_.IndexOf('='); $k = $_.Substring(0, $i).Trim(); $v = $_.Substring($i + 1).Trim()
        Set-Item -Path "Env:$k" -Value $v
    }
}
$exe = '__BINPATH__'
if (-not (Test-Path $exe)) { Write-Host "找不到 $exe" -ForegroundColor Red; exit 1 }
& $exe
'@ -replace '__BINPATH__', ($BinPath -replace '\\', '\\')
    Set-Content -Path $start -Value $startContent -Encoding UTF8
    Info "已生成启动脚本: $start"
}
function Add-ToPath {
    if (-not $IsWindows -or $NoPath) { return }
    try {
        $p = [Environment]::GetEnvironmentVariable("Path", "User")
        if ($p -notlike "*$InstallDir*") { [Environment]::SetEnvironmentVariable("Path", "$p;$InstallDir", "User"); Info "已加入用户 PATH（重开终端生效）" }
    } catch { Warn "无法自动加入 PATH，请手动添加 $InstallDir" }
}
function Do-Uninstall {
    Remove-Item -Path (Join-Path $InstallDir "$Bin.exe") -Force -ErrorAction SilentlyContinue
    Remove-Item -Path (Join-Path $ConfigDir "start-matrix-satori.ps1") -Force -ErrorAction SilentlyContinue
    if ($IsWindows) {
        try {
            $p = [Environment]::GetEnvironmentVariable("Path", "User")
            if ($p -like "*$InstallDir*") { $p2 = ($p -split ';' | Where-Object { $_ -and $_ -ne $InstallDir }) -join ';'; [Environment]::SetEnvironmentVariable("Path", $p2, "User") }
        } catch {}
    }
    Info "已卸载 $InstallDir\$Bin.exe 与启动脚本（配置目录 $ConfigDir 保留）"
}

if ($Uninstall) { Do-Uninstall; exit 0 }
if ($FromSource) { Install-FromSource } else { $Version = Resolve-Version; Install-FromRelease }
Write-Config
Add-ToPath
Write-Host ""
Info "安装完成!"
Info "  二进制: $BinPath"
Info "  配置:   $(Join-Path $ConfigDir 'config.env')"
Info "  数据:   $(Join-Path $ConfigDir 'data')"
Info "  Koishi: adapter-satori 的 endpoint 填 $Bind 对应地址，token 填: $Token"
Info "  启动:   $(Join-Path $ConfigDir 'start-matrix-satori.ps1')"
