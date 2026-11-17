# Offline Windows installer integration tests. Run with powershell or pwsh.
$ErrorActionPreference = 'Stop'
$installer = Join-Path (Split-Path -Parent $PSScriptRoot) 'install.ps1'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('lazymerge-installer-test-' + [Guid]::NewGuid().ToString('N'))
$variables = @('LAZYMERGE_INSTALL_DIR', 'LAZYMERGE_VERSION', 'LAZYMERGE_NO_MODIFY_PATH',
    'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432', 'LAZYMERGE_TEST_FIXTURES')
$saved = @{}
foreach ($name in $variables) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$global:LazyMergeTestRequests = @()

function Assert-InstallerTest([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function global:Invoke-WebRequest {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    $global:LazyMergeTestRequests += $Uri
    $name = ([Uri]$Uri).Segments[-1]
    Copy-Item -LiteralPath (Join-Path $env:LAZYMERGE_TEST_FIXTURES $name) -Destination $OutFile
}
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    New-Item -ItemType Directory -Path $temporary | Out-Null
    $fixtures = Join-Path $temporary 'release assets'
    New-Item -ItemType Directory -Path $fixtures | Out-Null
    $env:LAZYMERGE_TEST_FIXTURES = $fixtures
    $env:LAZYMERGE_INSTALL_DIR = Join-Path $temporary 'my tools\bin'
    $env:LAZYMERGE_NO_MODIFY_PATH = '1'
    $env:PROCESSOR_ARCHITEW6432 = ''
    $env:LAZYMERGE_VERSION = 'v0.1.1'
    $checksums = @()
    foreach ($target in @('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')) {
        $source = Join-Path $temporary $target
        New-Item -ItemType Directory -Path $source | Out-Null
        [IO.File]::WriteAllText((Join-Path $source 'lazymerge.exe'), "test executable $target")
        $archive = Join-Path $fixtures "lazymerge-$target.zip"
        [IO.Compression.ZipFile]::CreateFromDirectory($source, $archive)
        $checksums += (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash + "  lazymerge-$target.zip"
    }
    $manifest = Join-Path $fixtures 'SHA256SUMS'
    Set-Content -LiteralPath $manifest -Value $checksums -Encoding ASCII
    foreach ($architecture in @('AMD64', 'ARM64')) {
        $env:PROCESSOR_ARCHITECTURE = $architecture
        & $installer
        $destination = Join-Path $env:LAZYMERGE_INSTALL_DIR 'lazymerge.exe'
        $target = if ($architecture -eq 'AMD64') { 'x86_64-pc-windows-msvc' } else { 'aarch64-pc-windows-msvc' }
        Assert-InstallerTest ([IO.File]::ReadAllText($destination) -eq "test executable $target") 'Incorrect executable installed.'
    }
    Assert-InstallerTest ($global:LazyMergeTestRequests[0] -like '*/releases/download/v0.1.1/*') 'Pinned version was ignored.'
    $env:LAZYMERGE_VERSION = ''
    & $installer
    Assert-InstallerTest ($global:LazyMergeTestRequests[-1] -like '*/releases/latest/download/*') 'Latest release was ignored.'
    $tokens = $null
    $parseErrors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($installer, [ref]$tokens, [ref]$parseErrors)
    Assert-InstallerTest ($parseErrors.Count -eq 0) 'Installer has PowerShell syntax errors.'
    $pathFunction = $ast.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Add-LazyMergePath'
    }, $true)
    Invoke-Expression $pathFunction.Extent.Text
    $testDirectory = 'C:\Users\example\LazyMerge\bin'
    $updatedPath = Add-LazyMergePath -Value "C:\older;$testDirectory\;C:\other;$($testDirectory.ToUpperInvariant())" -Directory $testDirectory
    Assert-InstallerTest ($updatedPath -ceq "$testDirectory;C:\older;C:\other") 'PATH updates must prepend and deduplicate the install directory.'
    Assert-InstallerTest ((Add-LazyMergePath -Value $updatedPath -Directory $testDirectory) -ceq $updatedPath) 'PATH updates must be idempotent.'
    $before = [IO.File]::ReadAllText($destination)
    Set-Content -LiteralPath $manifest -Value (('0' * 64) + '  lazymerge-aarch64-pc-windows-msvc.zip') -Encoding ASCII
    $rejected = $false
    try { & $installer } catch { $rejected = $_.Exception.Message -like '*Checksum mismatch*' }
    Assert-InstallerTest $rejected 'Corrupted archive checksum was accepted.'
    Assert-InstallerTest ([IO.File]::ReadAllText($destination) -eq $before) 'Failed update changed the installed executable.'
    $env:LAZYMERGE_VERSION = '../../invalid'
    $count = $global:LazyMergeTestRequests.Count
    $rejected = $false
    try { & $installer } catch { $rejected = $_.Exception.Message -like '*LAZYMERGE_VERSION*' }
    Assert-InstallerTest $rejected 'Invalid version was accepted.'
    Assert-InstallerTest ($global:LazyMergeTestRequests.Count -eq $count) 'Invalid version initiated a download.'
    Assert-InstallerTest ([Environment]::GetEnvironmentVariable('Path', 'User') -ceq $userPath) 'Tests changed the user PATH.'
    Assert-InstallerTest (@(Get-ChildItem -LiteralPath $env:LAZYMERGE_INSTALL_DIR -Force).Count -eq 1) 'Installer left staging files behind.'
    Write-Host 'Windows installer tests passed: architectures, pinned version, spaces, updates, checksum failure and PATH isolation.'
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
    Remove-Item Function:\Invoke-WebRequest -ErrorAction SilentlyContinue
    Remove-Variable LazyMergeTestRequests -Scope Global -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Recurse -Force }
}
