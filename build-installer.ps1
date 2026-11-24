# Builds SCloud and packs it into the installer dist\SCloud-Setup-<version>.exe.
# That file is what you share: end users need neither Rust, Visual Studio nor setup.bat.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location $PSScriptRoot
function Step($m) { Write-Host "`n==> $m" -ForegroundColor Yellow }

try {
    # 1. build the app (libmpv, ffmpeg, fonts, same as setup.bat)
    & (Join-Path $PSScriptRoot 'setup.ps1') -NoRun
    if ($LASTEXITCODE -ne 0) { throw "Building the app failed, see the error above" }

    # 2. Inno Setup
    Step "Looking for Inno Setup"
    $candidates = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    )
    $iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) {
        Write-Host "Not found, installing (once)..."
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            winget install -e --id JRSoftware.InnoSetup --accept-package-agreements --accept-source-agreements
        } elseif (Get-Command choco -ErrorAction SilentlyContinue) {
            choco install innosetup -y --no-progress
        } else {
            throw "Install Inno Setup 6 manually: https://jrsoftware.org/isdl.php"
        }
        $iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
        if (-not $iscc) { throw "Inno Setup was installed but ISCC.exe was not found" }
    }
    Write-Host $iscc

    # 3. version from Cargo.toml
    $ver = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

    Step "Building installer SCloud-Setup-$ver.exe"
    & $iscc "/DMyAppVersion=$ver" (Join-Path $PSScriptRoot 'installer\scloud.iss')
    if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }

    $out = Join-Path $PSScriptRoot "dist\SCloud-Setup-$ver.exe"
    $size = [math]::Round((Get-Item $out).Length / 1MB, 1)
    Step "Done: dist\SCloud-Setup-$ver.exe ($size MB) is the file to share"
    if (-not $env:CI) { Start-Process explorer.exe "/select,`"$out`"" }
}
catch {
    Write-Host "`nERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
