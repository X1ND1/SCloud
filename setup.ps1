# Builds SCloud from source.
# Heavy dependencies (libmpv, ffmpeg, compiled Rust crates) are cached in %LOCALAPPDATA%\SCloud,
# so unpacking a new version into any folder doesn't re-download or recompile everything.
param(
    # do not launch the app after building (used by the installer build / CI)
    [switch]$NoRun
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Set-Location $PSScriptRoot
function Step($m) { Write-Host "`n==> $m" -ForegroundColor Yellow }

$cache = Join-Path $env:LOCALAPPDATA 'SCloud'
$deps = Join-Path $cache 'deps'
New-Item -ItemType Directory -Force $deps | Out-Null

try {
    Step "Checking Rust"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $env:Path += ";$env:USERPROFILE\.cargo\bin" }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        # started "as administrator": Rust lives in the regular user profile
        $found = Get-ChildItem 'C:\Users\*\.cargo\bin\cargo.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) {
            Write-Host "Found Rust: $($found.DirectoryName)"
            $env:Path += ";$($found.DirectoryName)"
            if (-not $env:RUSTUP_HOME) { $env:RUSTUP_HOME = Join-Path (Split-Path $found.DirectoryName -Parent | Split-Path -Parent) '.rustup' }
            if (-not $env:CARGO_HOME) { $env:CARGO_HOME = Split-Path $found.DirectoryName -Parent }
        }
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "Rust not found. Install it from https://rustup.rs and run setup.bat with a normal double-click (not as administrator)"
    }
    cargo --version

    Step "Checking Visual Studio Build Tools (C++)"
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vc = $null
    if (Test-Path $vswhere) {
        $vc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    }
    if ($vc) {
        Write-Host "Found: $vc"
    } else {
        Write-Host "Not found. Installing via winget (2-6 GB, Windows will ask for confirmation)..."
        winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
        if ($LASTEXITCODE -ne 0) {
            throw "Could not install Build Tools. Install them manually: https://visualstudio.microsoft.com/visual-cpp-build-tools/ (select 'Desktop development with C++')"
        }
    }

    # ---------- libmpv (playback) ----------
    $mpvDll = Join-Path $deps 'libmpv-2.dll'
    $mpvLib = Join-Path $deps 'mpv.lib'
    # reuse files already downloaded into the project folder (older setup versions)
    if (-not (Test-Path $mpvDll) -and (Test-Path "$PSScriptRoot\libmpv-2.dll")) { Copy-Item "$PSScriptRoot\libmpv-2.dll" $mpvDll }
    if (-not (Test-Path $mpvLib) -and (Test-Path "$PSScriptRoot\lib\mpv.lib")) { Copy-Item "$PSScriptRoot\lib\mpv.lib" $mpvLib }
    if ((Test-Path $mpvDll) -and (Test-Path $mpvLib)) {
        Step "libmpv already downloaded, skipping"
    } else {
        Step "Downloading libmpv (shinchiro/mpv-winbuild-cmake)"
        $rel = Invoke-RestMethod 'https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest' -Headers @{ 'User-Agent' = 'scloud-setup' }
        $asset = $rel.assets | Where-Object { $_.name -match '^mpv-dev-x86_64-\d' } | Select-Object -First 1
        if (-not $asset) { throw "mpv-dev-x86_64 not found in release $($rel.tag_name)" }
        Write-Host "$($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)"
        $archive = Join-Path $env:TEMP $asset.name
        Invoke-WebRequest $asset.browser_download_url -OutFile $archive

        Step "Extracting"
        $out = Join-Path $env:TEMP 'scloud-mpv-dev'
        Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory $out | Out-Null
        $sz = "$env:ProgramFiles\7-Zip\7z.exe"
        $ok = $false
        if (Test-Path $sz) { & $sz x $archive "-o$out" -y | Out-Null; $ok = ($LASTEXITCODE -eq 0) }
        if (-not $ok) {
            try { & tar.exe -xf $archive -C $out 2>&1 | Out-Null; $ok = ($LASTEXITCODE -eq 0) } catch { $ok = $false }
        }
        if (-not $ok) {
            Write-Host "Installing 7-Zip via winget..."
            winget install -e --id 7zip.7zip --accept-package-agreements --accept-source-agreements
            if (Test-Path $sz) { & $sz x $archive "-o$out" -y | Out-Null; $ok = ($LASTEXITCODE -eq 0) }
        }
        if (-not $ok) { throw "Could not extract $archive. Install 7-Zip (https://www.7-zip.org) and run again." }
        $dll = Get-ChildItem $out -Recurse -Filter 'libmpv-2.dll' | Select-Object -First 1
        $imp = Get-ChildItem $out -Recurse -Filter 'libmpv.dll.a' | Select-Object -First 1
        if (-not $dll -or -not $imp) { throw "The archive has no libmpv-2.dll or libmpv.dll.a" }
        Copy-Item $dll.FullName $mpvDll -Force
        Copy-Item $imp.FullName $mpvLib -Force
        Remove-Item $archive, $out -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Done"
    }

    # ---------- ffmpeg (MP3 downloads only) ----------
    $ffmpeg = Join-Path $deps 'ffmpeg.exe'
    if (-not (Test-Path $ffmpeg) -and (Test-Path "$PSScriptRoot\ffmpeg.exe")) { Copy-Item "$PSScriptRoot\ffmpeg.exe" $ffmpeg }
    if (-not (Test-Path $ffmpeg)) {
        $sys = Get-Command ffmpeg -ErrorAction SilentlyContinue
        if ($sys) { Write-Host "Found installed ffmpeg: $($sys.Source)"; Copy-Item $sys.Source $ffmpeg }
    }
    if (Test-Path $ffmpeg) {
        Step "ffmpeg already present, skipping"
    } else {
        Step "Downloading ffmpeg for MP3 downloads (BtbN/FFmpeg-Builds, ~150 MB, once)"
        $zip = Join-Path $env:TEMP 'scloud-ffmpeg.zip'
        Invoke-WebRequest 'https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip' -OutFile $zip
        $fdir = Join-Path $env:TEMP 'scloud-ffmpeg'
        Remove-Item $fdir -Recurse -Force -ErrorAction SilentlyContinue
        Expand-Archive $zip -DestinationPath $fdir -Force
        $exe = Get-ChildItem $fdir -Recurse -Filter 'ffmpeg.exe' | Select-Object -First 1
        if (-not $exe) { throw "The ffmpeg archive has no ffmpeg.exe" }
        Copy-Item $exe.FullName $ffmpeg -Force
        Remove-Item $zip, $fdir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Done"
    }

    # ---------- fonts (Unbounded for headings, Onest for the UI; both include Cyrillic) ----------
    $fonts = Join-Path $deps 'fonts'
    New-Item -ItemType Directory -Force $fonts | Out-Null
    $fontList = @(
        @('onest', 'latin', 400), @('onest', 'cyrillic', 400),
        @('onest', 'latin', 600), @('onest', 'cyrillic', 600),
        @('unbounded', 'latin', 700), @('unbounded', 'cyrillic', 700)
    )
    $missing = $fontList | Where-Object { -not (Test-Path (Join-Path $fonts "$($_[0])-$($_[1])-$($_[2]).ttf")) }
    if ($missing) {
        Step "Downloading fonts (Fontsource, ~0.5 MB)"
        foreach ($f in $missing) {
            $name = "$($f[0])-$($f[1])-$($f[2]).ttf"
            $url = "https://cdn.jsdelivr.net/fontsource/fonts/$($f[0])@latest/$($f[1])-$($f[2])-normal.ttf"
            try { Invoke-WebRequest $url -OutFile (Join-Path $fonts $name) }
            catch { Write-Host "Could not download $name, the default font will be used" -ForegroundColor DarkYellow }
        }
    } else {
        Step "Fonts already present, skipping"
    }

    # ---------- build ----------
    Step "Building SCloud (a few minutes the first time, seconds afterwards)"
    $env:MPV_LIB_DIR = $deps
    $env:CARGO_TARGET_DIR = Join-Path $cache 'target'   # shared build cache for any project folder
    # rustc can crash with 0xc0000409 (stack overflow) on very large UI functions,
    # so give the compiler a bigger stack
    $env:RUST_MIN_STACK = '268435456'
    & cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Build failed, retrying once (the compiler sometimes runs out of memory)" -ForegroundColor DarkYellow
        & cargo build --release
    }
    if ($LASTEXITCODE -ne 0) { throw "Build failed. See the error above." }

    $app = Join-Path $PSScriptRoot 'app'
    New-Item -ItemType Directory -Force $app | Out-Null
    Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\scloud.exe') $app -Force
    Copy-Item $mpvDll, $ffmpeg $app -Force
    New-Item -ItemType Directory -Force (Join-Path $app 'fonts') | Out-Null
    Copy-Item (Join-Path $fonts '*.ttf') (Join-Path $app 'fonts') -Force -ErrorAction SilentlyContinue

    if ($NoRun) {
        Step "Done! The app is in the app\ folder"
    } else {
        Step "Done! The app is in the app\ folder, launching app\scloud.exe"
        Start-Process (Join-Path $app 'scloud.exe')
    }
}
catch {
    Write-Host "`nERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
