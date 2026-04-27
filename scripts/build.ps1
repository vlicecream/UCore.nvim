param(
  [switch]$Clean
)

$ErrorActionPreference = "Stop"

$script_dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$plugin_root = Split-Path -Parent $script_dir
$scanner_dir = Join-Path $plugin_root "u-scanner"
$manifest_path = Join-Path $scanner_dir "Cargo.toml"

function Get-VsDevCmd {
  if ($env:VSINSTALLDIR) {
    $candidate = Join-Path $env:VSINSTALLDIR "Common7\Tools\VsDevCmd.bat"
    if (Test-Path $candidate) {
      return $candidate
    }
  }

  $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
  if (-not (Test-Path $vswhere)) {
    return $null
  }

  $install_path = & $vswhere `
    -latest `
    -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath

  if (-not $install_path) {
    return $null
  }

  $candidate = Join-Path $install_path "Common7\Tools\VsDevCmd.bat"
  if (Test-Path $candidate) {
    return $candidate
  }

  return $null
}

function Invoke-UCoreCargoBuild {
  param(
    [Parameter(Mandatory = $true)]
    [string]$LogPath
  )

  $vs_dev_cmd = Get-VsDevCmd
  $cargo_command = "cargo build --release --manifest-path `"$manifest_path`" --bin u_core_server --bin u_scanner"

  if ($vs_dev_cmd) {
    $command = "call `"$vs_dev_cmd`" -arch=x64 -host_arch=x64 >nul && " +
      "set `"CC_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"CXX_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"AR_x86_64_pc_windows_msvc=lib.exe`" && " +
      $cargo_command
  } elseif (Get-Command cl.exe -ErrorAction SilentlyContinue) {
    $command = "set `"CC_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"CXX_x86_64_pc_windows_msvc=cl.exe`" && " +
      "set `"AR_x86_64_pc_windows_msvc=lib.exe`" && " +
      $cargo_command
  } else {
    throw "MSVC C++ Build Tools were not found. Install Visual Studio Build Tools with the C++ workload."
  }

  Write-Host "UCore build: building Rust backend with MSVC tools"
  Write-Host $cargo_command

  $previous_error_action = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    Remove-Item -LiteralPath $LogPath -ErrorAction SilentlyContinue
    & cmd.exe /d /s /c ($command + " 2>&1") | ForEach-Object {
      Write-Host $_
      Add-Content -LiteralPath $LogPath -Value $_
    }
    return $LASTEXITCODE
  } finally {
    $ErrorActionPreference = $previous_error_action
  }
}

if (-not (Test-Path $manifest_path)) {
  throw "Cargo.toml not found: $manifest_path"
}

if ($Clean) {
  Write-Host "UCore build: cleaning backend target directory"
  cargo clean --manifest-path "$manifest_path"
}

$log_path = Join-Path ([System.IO.Path]::GetTempPath()) ("ucore-build-" + [System.Guid]::NewGuid().ToString("N") + ".log")
$exit_code = Invoke-UCoreCargoBuild -LogPath $log_path

if ($exit_code -ne 0) {
  $log_text = ""
  if (Test-Path $log_path) {
    $log_text = Get-Content -Raw -Path $log_path
  }

  if ($log_text -match "__mingw_|___chkstk_ms|LNK1120") {
    Write-Host ""
    Write-Host "UCore build: detected stale MinGW/GCC-built C objects; cleaning and retrying once."
    cargo clean --manifest-path "$manifest_path"
    $exit_code = Invoke-UCoreCargoBuild -LogPath $log_path
  }
}

Remove-Item -LiteralPath $log_path -ErrorAction SilentlyContinue
exit $exit_code
