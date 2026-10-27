$ErrorActionPreference = 'Stop'

$version = $env:GITHUB_REF_NAME.TrimStart('v')
if (-not $version) { throw "Expected a versioned release tag, got $env:GITHUB_REF_NAME" }
$currentName = 'rust-vlc-windows-x86_64-setup.exe'
$oldName = 'rust-vlc-windows-x86_64-setup.exe'
$currentPath = Join-Path $env:GITHUB_WORKSPACE "dist\$currentName"
$databaseScript = Join-Path $PSScriptRoot 'validate-install-database.py'
$tempRoot = Join-Path $env:RUNNER_TEMP ("rust-vlc-install-validation-" + [guid]::NewGuid().ToString('N'))
$oldDir = Join-Path $tempRoot 'old-release'
New-Item -ItemType Directory -Force -Path $oldDir | Out-Null

function Assert-Checksum([string] $file, [string] $checksumFile) {
    $name = [IO.Path]::GetFileName($file)
    $record = (Get-Content -LiteralPath $checksumFile -Raw).Trim()
    if ($record -notmatch '^([0-9a-fA-F]{64})\s{2}(.+)$') { throw "Malformed SHA-256 sidecar for $name" }
    $expectedHash = $Matches[1]
    if ($Matches[2] -ne $name) { throw "SHA-256 sidecar names the wrong file: $name" }
    $actual = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expectedHash.ToLowerInvariant()) { throw "SHA-256 mismatch for $name" }
}

function Download-OldInstaller {
    & gh release download v0.1.2 --repo $env:GITHUB_REPOSITORY --pattern $oldName --pattern "$oldName.sha256" --dir $oldDir
    if ($LASTEXITCODE -ne 0) { throw 'Could not download the published v0.1.2 installer and checksum' }
    Assert-Checksum (Join-Path $oldDir $oldName) (Join-Path $oldDir "$oldName.sha256")
    Assert-Checksum $currentPath "$currentPath.sha256"
}

function Install-Setup([string] $installer, [string] $installDir) {
    $arguments = @('/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=`"$installDir`"", '/NOICONS')
    $process = Start-Process -FilePath $installer -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer exited with $($process.ExitCode): $installer" }
}

function Assert-Installed([string] $installDir, [string] $expectedVersion, [string] $expectedCommit = '') {
    $executable = Join-Path $installDir 'rust-vlc.exe'
    $infoPath = Join-Path $installDir 'BUILD-INFO.txt'
    $iconPath = Join-Path $installDir 'rust-vlc.ico'
    $vlcPath = Join-Path $installDir 'VLC'
    $pluginPath = Join-Path $vlcPath 'plugins'
    foreach ($required in @($executable, $infoPath, $iconPath, (Join-Path $vlcPath 'libvlc.dll'), (Join-Path $vlcPath 'libvlccore.dll'))) {
        if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Installed package is missing $required" }
    }
    if (-not (Test-Path -LiteralPath $pluginPath -PathType Container)) { throw 'Installed package is missing VLC plugins' }
    if (@(Get-ChildItem -LiteralPath $pluginPath -Filter '*.dll' -File -Recurse).Count -eq 0) { throw 'Installed package contains no VLC plugin DLLs' }
    if ((Get-Item -LiteralPath $iconPath).Length -eq 0) { throw 'Installed package has an empty icon' }

    $exeVersion = (Get-Item -LiteralPath $executable).VersionInfo
    if ($exeVersion.ProductName -ne 'Rust-VLC' -or $exeVersion.ProductVersion -ne $expectedVersion) {
        throw "Installed executable version metadata is unexpected: $($exeVersion.ProductName) $($exeVersion.ProductVersion)"
    }
    $buildInfo = Get-Content -LiteralPath $infoPath -Raw
    if ($buildInfo -notmatch "(?m)^Version: $([regex]::Escape($expectedVersion))$") { throw "BUILD-INFO.txt does not report $expectedVersion" }
    if ($buildInfo -notmatch '(?m)^Target: x86_64-pc-windows-msvc$' -or $buildInfo -notmatch '(?m)^Source commit: [0-9a-f]{40}$') {
        throw 'BUILD-INFO.txt is missing the expected target or source commit'
    }
    if ($expectedCommit -and $buildInfo -notmatch "(?m)^Source commit: $([regex]::Escape($expectedCommit))$") {
        throw 'BUILD-INFO.txt does not identify this release source commit'
    }
}

function New-DataSentinel([string] $name) {
    $dataDir = Join-Path $env:LOCALAPPDATA 'benclawbot\Rust-VLC\data'
    New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
    $path = Join-Path $dataDir $name
    [IO.File]::WriteAllBytes($path, [Text.Encoding]::UTF8.GetBytes("synthetic Rust-VLC retention sentinel: $name`n"))
    return @{ Path = $path; Hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
}

function Assert-Sentinel([hashtable] $sentinel) {
    if (-not (Test-Path -LiteralPath $sentinel.Path -PathType Leaf)) { throw "Installer removed user data: $($sentinel.Path)" }
    $actual = (Get-FileHash -LiteralPath $sentinel.Path -Algorithm SHA256).Hash
    if ($actual -ne $sentinel.Hash) { throw "Installer changed user data: $($sentinel.Path)" }
}

function New-LibraryDatabase([string] $path) {
    $output = & python $databaseScript create $path
    if ($LASTEXITCODE -ne 0) { throw 'Could not create the synthetic library database' }
    return ($output | Select-Object -Last 1).ToString().Trim()
}

function Assert-LibraryDatabase([string] $path, [string] $expectedHash) {
    & python $databaseScript verify $path $expectedHash
    if ($LASTEXITCODE -ne 0) { throw 'The installed package or uninstaller changed the library database' }
}

function Uninstall-Setup([string] $installDir) {
    $uninstaller = Get-ChildItem -LiteralPath $installDir -Filter 'unins*.exe' -File | Select-Object -First 1
    if ($null -eq $uninstaller) { throw "Inno Setup uninstaller is missing from $installDir" }
    $process = Start-Process -FilePath $uninstaller.FullName -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Uninstaller exited with $($process.ExitCode)" }
    if (Test-Path -LiteralPath (Join-Path $installDir 'rust-vlc.exe')) { throw 'Uninstaller left the application executable behind' }
}

Download-OldInstaller
$oldPath = Join-Path $oldDir $oldName

# Verify a clean current-version install and that uninstall leaves user data alone.
$cleanDir = Join-Path $tempRoot 'clean-install'
Install-Setup $currentPath $cleanDir
Assert-Installed $cleanDir $version $env:GITHUB_SHA
$cleanSentinel = New-DataSentinel 'clean-install-retention.sentinel'
Uninstall-Setup $cleanDir
Assert-Sentinel $cleanSentinel

# Install the authenticated published package, upgrade in place, then uninstall.
$upgradeDir = Join-Path $tempRoot 'upgrade-install'
Install-Setup $oldPath $upgradeDir
Assert-Installed $upgradeDir '0.1.2'
$databasePath = Join-Path $env:LOCALAPPDATA 'benclawbot\Rust-VLC\data\library.sqlite3'
$databaseHash = New-LibraryDatabase $databasePath
$upgradeSentinel = New-DataSentinel 'upgrade-retention.sentinel'
Install-Setup $currentPath $upgradeDir
Assert-Installed $upgradeDir $version $env:GITHUB_SHA
Assert-Sentinel $cleanSentinel
Assert-Sentinel $upgradeSentinel
Assert-LibraryDatabase $databasePath $databaseHash
Uninstall-Setup $upgradeDir
Assert-Sentinel $cleanSentinel
Assert-Sentinel $upgradeSentinel
Assert-LibraryDatabase $databasePath $databaseHash

Write-Host 'Windows clean install, v0.1.2-to-current upgrade, package inventory, synthetic library database/sentinel retention through upgrade and uninstall passed. GUI launch and playback were not exercised.'
