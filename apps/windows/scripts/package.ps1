# Builds the Windows installer, dist\InputWeave-<version>-<arch>.msi: the
# background service (cargo), the notification area app (dotnet publish)
# and the MSI around both (WiX 5, packaging/windows/InputWeave.wxs). CI runs
# this too, so a local package is built the same way.
#
#   powershell -ExecutionPolicy Bypass -File apps\windows\scripts\package.ps1 [-Arch x64|arm64]
#
# Run it from anywhere; it works in the repository root. Needs the Rust
# target of the architecture, the .NET 9 SDK, and WiX 5.0.2 with its
# Firewall and Util extensions (see apps/windows/README.md).

param(
    [ValidateSet('x64', 'arm64')]
    [string]$Arch = 'x64'
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
Set-Location $root

function Run([string]$File, [string[]]$Arguments) {
    Write-Host "> $File $($Arguments -join ' ')"
    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$File exited with $LASTEXITCODE"
    }
}

$triple = @{ x64 = 'x86_64-pc-windows-msvc'; arm64 = 'aarch64-pc-windows-msvc' }[$Arch]
$rid = @{ x64 = 'win-x64'; arm64 = 'win-arm64' }[$Arch]

# The MSI takes the Cargo workspace version, like the app does
# (apps/windows/Directory.Build.props).
$manifest = Get-Content -Raw Cargo.toml
if ($manifest -notmatch '(?m)^\[workspace\.package\][^\[]*?^version = "([0-9]+\.[0-9]+\.[0-9]+)"') {
    throw 'No version = "x.y.z" under [workspace.package] in Cargo.toml'
}
$version = $Matches[1]

Run cargo @('build', '--release', '-p', 'inputweave', '--target', $triple)

# A fresh folder: the MSI takes every file in it.
$app = Join-Path 'target\app' $rid
if (Test-Path $app) {
    Remove-Item -Recurse -Force $app
}
Run dotnet @('publish', 'apps\windows\InputWeave\InputWeave.csproj', '-c', 'Release', '-r', $rid, '--self-contained', '-o', $app)

# The bind path must be absolute: WiX looks for the files to harvest
# relative to the .wxs, not to the current directory. Warnings are errors,
# so a folder it cannot find fails the build instead of leaving the app out.
$msi = "dist\InputWeave-$version-$Arch.msi"
Run wix @('build', 'packaging\windows\InputWeave.wxs', '-arch', $Arch,
    '-ext', 'WixToolset.Firewall.wixext', '-ext', 'WixToolset.Util.wixext',
    '-d', "Version=$version", '-d', "BinDir=target\$triple\release",
    '-bindpath', "App=$((Resolve-Path $app).Path)",
    '-wx', '-o', $msi)

# Beside the MSI, what scripts/release.sh checks before publishing it, since
# the Mac it runs on cannot read an MSI: the ProductVersion the MSI carries,
# the build number (apps/macos/scripts/build-number.sh's rule), and the
# MSI's SHA-256 and size. Copy both files from this machine; the .json is
# not published.
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $msi).Path, 0))
$view = $database.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $database,
    @("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = 'ProductVersion'"))
$view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
$record = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
$productVersion = $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, 1)
$view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
[void][Runtime.InteropServices.Marshal]::ReleaseComObject($database)
$parts = @($version.Split('.') | ForEach-Object { [int]$_ })
if ($parts[1] -gt 99 -or $parts[2] -gt 99) {
    throw "$version does not fit major * 10000 + minor * 100 + patch"
}
$sidecar = [ordered]@{
    product_version = $productVersion
    build = $parts[0] * 10000 + $parts[1] * 100 + $parts[2]
    sha256 = (Get-FileHash -Algorithm SHA256 $msi).Hash.ToLowerInvariant()
    size = (Get-Item $msi).Length
}
# UTF-8 without a byte order mark.
[IO.File]::WriteAllText((Join-Path (Get-Location) "$msi.json"), (ConvertTo-Json $sidecar) + "`n")
Write-Host "$msi.json: ProductVersion $productVersion, build $($sidecar.build)"

$appSize = (Get-ChildItem -Recurse -File $app | Measure-Object -Property Length -Sum).Sum
Write-Host ('{0}: {1:N1} MB (the app folder: {2:N1} MB)' -f $msi, ((Get-Item $msi).Length / 1MB), ($appSize / 1MB))
