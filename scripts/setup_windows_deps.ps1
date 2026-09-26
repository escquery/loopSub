[CmdletBinding()]
param(
    [ValidateSet("dev", "release", "bundle")]
    [string]$Mode = "dev",
    [switch]$ForceRefresh
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
switch ($Mode) {
    "dev"     { $Destination = Join-Path $Root "src-tauri\target\debug" }
    "release" { $Destination = Join-Path $Root "src-tauri\target\release" }
    "bundle"  { $Destination = Join-Path $Root "src-tauri\resources" }
}

$Cache = Join-Path $Root "src-tauri\target\windows-deps\x86_64"
if ($ForceRefresh -and (Test-Path $Cache)) {
    Remove-Item -Recurse -Force $Cache
}

function Find-SevenZip {
    $command = Get-Command 7z.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }

    $candidates = @(
        "$env:ProgramFiles\7-Zip\7z.exe",
        "${env:ProgramFiles(x86)}\7-Zip\7z.exe"
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) { return $candidate }
    }
    throw "7-Zip was not found. Install it with: winget install --id 7zip.7zip -e"
}

if (-not (Test-Path (Join-Path $Cache ".complete"))) {
    $SevenZip = Find-SevenZip
    $Work = Join-Path ([IO.Path]::GetTempPath()) ("loopsub-deps-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Force $Work | Out-Null
    try {
        Write-Host "Downloading ffmpeg and ffprobe..."
        $FfmpegZip = Join-Path $Work "ffmpeg.zip"
        Invoke-WebRequest "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" -OutFile $FfmpegZip
        $FfmpegDir = Join-Path $Work "ffmpeg"
        Expand-Archive -Path $FfmpegZip -DestinationPath $FfmpegDir -Force
        $Ffmpeg = Get-ChildItem $FfmpegDir -Recurse -File -Filter "ffmpeg.exe" |
            Select-Object -First 1
        $Ffprobe = Get-ChildItem $FfmpegDir -Recurse -File -Filter "ffprobe.exe" |
            Select-Object -First 1
        if (-not $Ffmpeg -or -not $Ffprobe) {
            throw "ffmpeg archive does not contain ffmpeg.exe and ffprobe.exe"
        }

        Write-Host "Discovering the latest x86_64 libmpv development package..."
        $Headers = @{ "User-Agent" = "loopSub dependency setup" }
        $Release = Invoke-RestMethod "https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest" -Headers $Headers
        $Asset = $Release.assets |
            Where-Object { $_.name -match '^mpv-dev-x86_64-\d+-git-[0-9a-f]+\.7z$' } |
            Select-Object -First 1
        if (-not $Asset) {
            throw "The latest shinchiro release does not contain an x86_64 mpv-dev package"
        }
        Write-Host "Downloading $($Asset.name)..."
        $MpvArchive = Join-Path $Work "mpv-dev.7z"
        Invoke-WebRequest $Asset.browser_download_url -OutFile $MpvArchive -Headers $Headers
        $MpvDir = Join-Path $Work "mpv"
        New-Item -ItemType Directory -Force $MpvDir | Out-Null
        & $SevenZip x $MpvArchive "-o$MpvDir" -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "7-Zip failed with exit code $LASTEXITCODE" }
        $Libmpv = Get-ChildItem $MpvDir -Recurse -File -Filter "libmpv-2.dll" |
            Select-Object -First 1
        if (-not $Libmpv) { throw "mpv-dev archive does not contain libmpv-2.dll" }

        New-Item -ItemType Directory -Force $Cache | Out-Null
        Copy-Item $Ffmpeg.FullName (Join-Path $Cache "ffmpeg.exe") -Force
        Copy-Item $Ffprobe.FullName (Join-Path $Cache "ffprobe.exe") -Force
        Copy-Item $Libmpv.FullName (Join-Path $Cache "libmpv-2.dll") -Force
        Set-Content -Path (Join-Path $Cache ".complete") -Value $Asset.name
    }
    finally {
        if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
    }
}

New-Item -ItemType Directory -Force $Destination | Out-Null
if ($Mode -eq "bundle") {
    # Avoid accidentally bundling native files left by a macOS build in a shared checkout.
    Get-ChildItem $Destination -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -eq ".dylib" -or $_.Name -in @("ffmpeg", "ffprobe") } |
        Remove-Item -Force
}
foreach ($name in @("libmpv-2.dll", "ffmpeg.exe", "ffprobe.exe")) {
    Copy-Item (Join-Path $Cache $name) (Join-Path $Destination $name) -Force
}

Write-Host "Dependencies installed in: $Destination"
$Installed = foreach ($name in @("libmpv-2.dll", "ffmpeg.exe", "ffprobe.exe")) {
    Get-Item (Join-Path $Destination $name)
}
$Installed | Select-Object Name, Length, FullName | Format-Table -AutoSize
& (Join-Path $Destination "ffmpeg.exe") -version | Select-Object -First 1
