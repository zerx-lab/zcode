# 本地构建产物 -> %LOCALAPPDATA%\zcode 的快速安装脚本。
#
# 与 scripts/install.ps1 的区别：那个从 GitHub release 下载官方二进制，这个装你
# 当前工作树编出来的 zcode.exe。native addon（.node）、stats 前端、collab-web
# tool-views、mupdf wasm 都由 packages/coding-agent/scripts/build-binary.ts 在
# 编译期嵌入单文件，因此这里只需要搬一个 exe。
#
# 用法：
#   pwsh brand/install-local.ps1              # 编译 + 安装
#   pwsh brand/install-local.ps1 -SkipBuild   # 直接装 dist/ 里已有的产物
#   pwsh brand/install-local.ps1 -NoPath      # 不动用户 PATH

param(
    [switch]$SkipBuild,
    [switch]$NoPath
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$AgentDir = Join-Path $RepoRoot "packages\coding-agent"
$BuiltExe = Join-Path $AgentDir "dist\zcode.exe"
$InstallDir = if ($env:ZCODE_INSTALL_DIR) { $env:ZCODE_INSTALL_DIR } else { "$env:LOCALAPPDATA\zcode" }
$TargetExe = Join-Path $InstallDir "zcode.exe"

if (-not $SkipBuild) {
    Write-Host "Building zcode binary (native + frontend embedded)..."
    Push-Location $AgentDir
    try {
        & bun run build
        if ($LASTEXITCODE -ne 0) { throw "bun run build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $BuiltExe)) {
    throw "Built binary not found: $BuiltExe`nRun without -SkipBuild first."
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# 正在运行的 exe 无法被覆盖，但可以被改名 —— 先挪走旧的再写新的。
#
# 改名目标必须唯一：上一次升级留下的 .old 往往仍被那次启动的进程映射着，删不掉，
# 固定名字会让 Move-Item 撞上一个锁住的目标并伪装成"新目标在运行"。
if (Test-Path $TargetExe) {
    $stale = "$TargetExe.old-$(Get-Date -Format yyyyMMddHHmmss)"
    try {
        Move-Item $TargetExe $stale
    } catch {
        throw "Cannot replace ${TargetExe}: $($_.Exception.Message)"
    }
}

Copy-Item -Force $BuiltExe $TargetExe

# 尽力回收历史残留；仍被占用的会留到下次运行（或重启后）再清掉。
Get-ChildItem -Path $InstallDir -Filter "zcode.exe.old*" -ErrorAction SilentlyContinue |
    ForEach-Object { Remove-Item -Force -ErrorAction SilentlyContinue $_.FullName }

$size = [math]::Round((Get-Item $TargetExe).Length / 1MB, 1)
Write-Host "[OK] Installed zcode ($size MB) to $TargetExe" -ForegroundColor Green

if (-not $NoPath) {
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($UserPath -notlike "*$InstallDir*") {
        Write-Host "Adding $InstallDir to PATH..."
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
        Write-Host "Restart your terminal, then run 'zcode'."
    } else {
        Write-Host "Run 'zcode' to get started!"
    }
}
