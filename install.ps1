# Install a published LazyMerge binary in Windows PowerShell 5.1 or newer.
# Optional environment variables: LAZYMERGE_VERSION, LAZYMERGE_INSTALL_DIR.
& {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version 2.0

    function Get-LazyMergeTarget {
        $architecture = $env:PROCESSOR_ARCHITEW6432
        if ([string]::IsNullOrWhiteSpace($architecture)) {
            $architecture = $env:PROCESSOR_ARCHITECTURE
        }
        switch ($architecture.ToUpperInvariant()) {
            'AMD64' { return 'x86_64-pc-windows-msvc' }
            'ARM64' { return 'aarch64-pc-windows-msvc' }
            default { throw "Unsupported Windows architecture: $architecture (supported: x64 and ARM64)." }
        }
    }

    function Add-LazyMergePath {
        param([string]$Value, [string]$Directory)
        $normalizedDirectory = $Directory.TrimEnd('\')
        $otherEntries = @($Value -split ';' | Where-Object {
            -not [string]::IsNullOrWhiteSpace($_) -and
            [Environment]::ExpandEnvironmentVariables($_.Trim().Trim('"')).TrimEnd('\') -ine $normalizedDirectory
        })
        return (@($Directory) + $otherEntries) -join ';'
    }

    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'On Linux and macOS, use install.sh.'
    }
    $target = Get-LazyMergeTarget
    $archiveName = "lazymerge-$target.zip"
    $version = $env:LAZYMERGE_VERSION
    if (-not [string]::IsNullOrWhiteSpace($version)) {
        if ($version -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$') {
            throw 'LAZYMERGE_VERSION must be a version such as 0.1.2.'
        }
        $version = $version -replace '^v', ''
        $release = "https://github.com/he-d/lazymerge/releases/download/v$version"
    } else {
        $release = 'https://github.com/he-d/lazymerge/releases/latest/download'
    }
    $installDir = $env:LAZYMERGE_INSTALL_DIR
    if ([string]::IsNullOrWhiteSpace($installDir)) {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            throw 'LOCALAPPDATA is not set. Set LAZYMERGE_INSTALL_DIR to your binary directory.'
        }
        $installDir = Join-Path $env:LOCALAPPDATA 'LazyMerge\bin'
    }
    $installDir = [IO.Path]::GetFullPath($installDir)
    $destination = Join-Path $installDir 'lazymerge.exe'
    $temporary = Join-Path ([IO.Path]::GetTempPath()) ('lazymerge-install-' + [Guid]::NewGuid().ToString('N'))
    $staged = $null
    $originalTls = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $originalTls -bor [Net.SecurityProtocolType]::Tls12
        New-Item -ItemType Directory -Path $temporary | Out-Null
        $archive = Join-Path $temporary $archiveName
        $checksums = Join-Path $temporary 'SHA256SUMS'
        Write-Host "Downloading LazyMerge for $target..."
        Invoke-WebRequest -UseBasicParsing -Uri "$release/$archiveName" -OutFile $archive
        Invoke-WebRequest -UseBasicParsing -Uri "$release/SHA256SUMS" -OutFile $checksums
        $pattern = '^([a-fA-F0-9]{64})\s+\*?' + [regex]::Escape($archiveName) + '$'
        $matching = @(Get-Content -LiteralPath $checksums | Where-Object { $_ -match $pattern })
        if ($matching.Count -ne 1) {
            throw "No unique checksum was found for $archiveName."
        }
        $null = $matching[0] -match $pattern
        $expected = $Matches[1]
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
        if ($actual -ine $expected) {
            throw 'Checksum mismatch; the existing installation was not changed.'
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($archive)
        try {
            $entry = $zip.GetEntry('lazymerge.exe')
            if ($null -eq $entry -or $entry.Length -eq 0) {
                throw 'The release archive does not contain lazymerge.exe.'
            }
            $candidate = Join-Path $temporary 'lazymerge.exe'
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $candidate, $false)
        } finally {
            $zip.Dispose()
        }
        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        if (Test-Path -LiteralPath $destination -PathType Container) {
            throw "$destination is a directory."
        }
        $staged = Join-Path $installDir ('.lazymerge-' + [Guid]::NewGuid().ToString('N') + '.exe')
        [IO.File]::Copy($candidate, $staged)
        try {
            if ([IO.File]::Exists($destination)) {
                [IO.File]::Replace($staged, $destination, [NullString]::Value)
            } else {
                [IO.File]::Move($staged, $destination)
            }
        } catch {
            throw "Could not update $destination. Close LazyMerge and rerun the installer. $($_.Exception.Message)"
        }
        $staged = $null
        # This switch is useful for custom directories and isolated installer tests.
        if ($env:LAZYMERGE_NO_MODIFY_PATH -ne '1') {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $newPath = Add-LazyMergePath -Value $userPath -Directory $installDir
            if ($newPath -cne $userPath) {
                [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            }
            $env:Path = Add-LazyMergePath -Value $env:Path -Directory $installDir
        }
        Write-Host "Installed LazyMerge to $destination"
        Write-Host 'Run: lazymerge (open a new terminal if needed).'
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $originalTls
        if ($null -ne $staged -and (Test-Path -LiteralPath $staged)) {
            Remove-Item -LiteralPath $staged -Force
        }
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Recurse -Force
        }
    }
}
