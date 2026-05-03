param(
  [switch]$Clean
)

$ErrorActionPreference = "Stop"

taskkill /f /im u_core_server.exe 2>$null
taskkill /f /im u_scanner.exe 2>$null

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$pluginRoot = Split-Path -Parent $scriptDir
$siblingSourceDir = [System.IO.Path]::GetFullPath((Join-Path $pluginRoot "..\UScanner"))

function Get-UCoreDataDir {
  if ($env:XDG_DATA_HOME) {
    return $env:XDG_DATA_HOME
  }

  $lazyRoot = Split-Path -Parent $pluginRoot
  if ((Split-Path -Leaf $lazyRoot) -eq "lazy") {
    return (Split-Path -Parent $lazyRoot)
  }

  if ($env:LOCALAPPDATA) {
    return (Join-Path $env:LOCALAPPDATA "nvim-data")
  }

  return $null
}

function Get-VsDevCmd {
  if ($env:VSINSTALLDIR) {
    $candidate = Join-Path $env:VSINSTALLDIR "Common7\Tools\VsDevCmd.bat"
    if (Test-Path -LiteralPath $candidate) {
      return $candidate
    }
  }

  $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
  if (-not (Test-Path -LiteralPath $vswhere)) {
    return $null
  }

  $installPath = & $vswhere `
    -latest `
    -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath

  if (-not $installPath) {
    return $null
  }

  $candidate = Join-Path $installPath "Common7\Tools\VsDevCmd.bat"
  if (Test-Path -LiteralPath $candidate) {
    return $candidate
  }

  return $null
}

function Resolve-BackendSource {
  $siblingManifest = Join-Path $siblingSourceDir "Cargo.toml"
  if (Test-Path -LiteralPath $siblingManifest) {
    return @{
      Path = $siblingSourceDir
      Mode = "sibling"
    }
  }

  throw "UScanner was not found. Expected sibling repo: $siblingSourceDir"
}

function Invoke-UCoreCargoBuild {
  param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true)]
    [string]$LogPath
  )

  $vsDevCmd = Get-VsDevCmd
  $cargoCommand = "cargo build --release --manifest-path `"$ManifestPath`" --bin u_core_server --bin u_scanner"

  if ($vsDevCmd) {
    $command = "call `"$vsDevCmd`" -arch=x64 -host_arch=x64 >nul && " +
      "set `"CC_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"CXX_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"AR_x86_64_pc_windows_msvc=lib.exe`" && " +
      $cargoCommand
  }
  elseif (Get-Command cl.exe -ErrorAction SilentlyContinue) {
    $command = "set `"CC_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"CXX_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"AR_x86_64_pc_windows_msvc=lib.exe`" && " +
      $cargoCommand
  }
  else {
    throw "MSVC C++ Build Tools were not found. Install Visual Studio Build Tools with the C++ workload."
  }

  Write-Host "UCore build: building UScanner backend with MSVC tools"
  Write-Host $cargoCommand

  $previousErrorAction = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    Remove-Item -LiteralPath $LogPath -ErrorAction SilentlyContinue
    & cmd.exe /d /s /c ($command + " 2>&1") | ForEach-Object {
      Write-Host $_
      Add-Content -LiteralPath $LogPath -Value $_
    }
    return $LASTEXITCODE
  }
  finally {
    $ErrorActionPreference = $previousErrorAction
  }
}

$backendSource = Resolve-BackendSource
$manifestPath = Join-Path $backendSource.Path "Cargo.toml"
Write-Host "UCore build: backend source = $($backendSource.Path) [$($backendSource.Mode)]"

if ($Clean) {
  Write-Host "UCore build: cleaning backend target directory"
  cargo clean --manifest-path $manifestPath
}

$logPath = Join-Path ([System.IO.Path]::GetTempPath()) ("ucore-build-" + [System.Guid]::NewGuid().ToString("N") + ".log")
$exitCode = Invoke-UCoreCargoBuild -ManifestPath $manifestPath -LogPath $logPath

if ($exitCode -ne 0) {
  $logText = ""
  if (Test-Path -LiteralPath $logPath) {
    $logText = Get-Content -Raw -LiteralPath $logPath
  }

  if ($logText -match "__mingw_|___chkstk_ms|LNK1120") {
    Write-Host ""
    Write-Host "UCore build: detected stale MinGW/GCC-built C objects; cleaning and retrying once."
    cargo clean --manifest-path $manifestPath
    $exitCode = Invoke-UCoreCargoBuild -ManifestPath $manifestPath -LogPath $logPath
  }
}

Remove-Item -LiteralPath $logPath -ErrorAction SilentlyContinue

if ($exitCode -eq 0) {
  $serverExe = Join-Path $backendSource.Path "target\release\u_core_server.exe"
  $dataDir = Get-UCoreDataDir
  $registry = if ($dataDir) { Join-Path $dataDir "ucore\server-registry.json" } else { $null }

  if ((Test-Path -LiteralPath $serverExe) -and $registry -and (Test-Path -LiteralPath $registry)) {
    Write-Host "UCore build: restarting server..."
    Start-Process -WindowStyle Hidden -FilePath $serverExe -ArgumentList "30110", $registry
    Start-Sleep -Milliseconds 500
  }
}

exit $exitCode
