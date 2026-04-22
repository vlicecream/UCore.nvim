param(
    [Parameter(Mandatory = $true)]
    [string]$Name,

    [int]$Port = 30110,

    [string]$TestDir = $PSScriptRoot
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $TestDir
$scannerDir = Join-Path $repoRoot "u-scanner"
$localDir = Join-Path $TestDir "local"

$fileName = if ($Name.EndsWith(".json")) { $Name } else { "$Name.json" }
$queryPath = Join-Path $localDir $fileName

if (-not (Test-Path -LiteralPath $queryPath)) {
    throw "Query file not found: $queryPath. Run make-local.ps1 first."
}

$oldPort = $env:UNL_SERVER_PORT
$env:UNL_SERVER_PORT = [string]$Port

Push-Location $scannerDir
try {
    $payload = Get-Content -LiteralPath $queryPath -Raw
    cargo run --bin u_scanner -- query $payload
}
finally {
    Pop-Location

    if ($null -eq $oldPort) {
        Remove-Item Env:\UNL_SERVER_PORT -ErrorAction SilentlyContinue
    }
    else {
        $env:UNL_SERVER_PORT = $oldPort
    }
}

