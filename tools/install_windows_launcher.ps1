[CmdletBinding()]
param(
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA 'Doujin Tagger'),
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$install = [System.IO.Path]::GetFullPath($InstallDirectory)

if (-not $SkipBuild) {
    & cargo build --release -p doujin-http -p doujin-launcher --manifest-path (Join-Path $repository 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Windows release build 失敗；未安裝 Launcher。' }
}

$release = Join-Path $repository 'target\release'
$httpSource = Join-Path $release 'doujin-http.exe'
$launcherSource = Join-Path $release 'doujin-launcher.exe'
$desktopSource = Join-Path $release 'doujin-tagger.exe'
if (-not (Test-Path -LiteralPath $httpSource -PathType Leaf) -or
    -not (Test-Path -LiteralPath $launcherSource -PathType Leaf) -or
    -not (Test-Path -LiteralPath $desktopSource -PathType Leaf)) {
    throw "找不到 release binaries：$release"
}

New-Item -ItemType Directory -Path $install -Force | Out-Null
Copy-Item -LiteralPath $httpSource -Destination (Join-Path $install 'doujin-http.exe') -Force
Copy-Item -LiteralPath $launcherSource -Destination (Join-Path $install 'doujin-launcher.exe') -Force
Copy-Item -LiteralPath $desktopSource -Destination (Join-Path $install '私藏編目室.exe') -Force

$shell = New-Object -ComObject WScript.Shell
$programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Doujin Tagger'
New-Item -ItemType Directory -Path $programs -Force | Out-Null
$launcher = Join-Path $install 'doujin-launcher.exe'
$desktop = Join-Path $install '私藏編目室.exe'

$shortcuts = @(
    @{ Name = '開啟私藏編目室'; Target = $desktop; Arguments = '' },
    @{ Name = '重新啟動私藏編目室'; Target = $launcher; Arguments = 'restart' },
    @{ Name = '停止私藏編目室'; Target = $launcher; Arguments = 'stop' },
    @{ Name = '私藏編目室服務狀態'; Target = $launcher; Arguments = 'status' }
)

foreach ($entry in $shortcuts) {
    $shortcut = $shell.CreateShortcut((Join-Path $programs ($entry.Name + '.lnk')))
    $shortcut.TargetPath = $entry.Target
    $shortcut.Arguments = $entry.Arguments
    $shortcut.WorkingDirectory = $install
    $shortcut.Description = $entry.Name
    $shortcut.Save()
}

$desktopShortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) '私藏編目室.lnk'))
$desktopShortcut.TargetPath = $desktop
$desktopShortcut.WorkingDirectory = $install
$desktopShortcut.Description = '開啟私藏編目室'
$desktopShortcut.Save()

Write-Host "Launcher 已安裝：$install"
Write-Host '桌面與開始功能表捷徑已建立。第一次開啟時可建立或選擇 v2 catalog。'
