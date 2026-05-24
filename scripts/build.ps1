param(
  [switch]$Clean
)

$ErrorActionPreference = "Stop"

Get-Process -Name u_core_server -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process -Name u_scanner -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$pluginRoot = Split-Path -Parent $scriptDir
$backendSource = Join-Path $pluginRoot "UScanner"
$manifestPath = Join-Path $backendSource "Cargo.toml"

if (-not (Test-Path -LiteralPath $manifestPath)) {
  throw "Bundled UScanner source was not found at $backendSource"
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

function Invoke-UCoreCargoBuild {
  param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true)]
    [string]$LogPath
  )

  $vsDevCmd = Get-VsDevCmd
  $cargoCommand = "cargo build --locked --release --manifest-path `"$ManifestPath`" --bin u_core_server --bin u_scanner"

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

  Write-Host "UCore build: building bundled UScanner backend with MSVC tools"
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

Write-Host "UCore build: backend source = $backendSource [bundled]"

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

exit $exitCode
