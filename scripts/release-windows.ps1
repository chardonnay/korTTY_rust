param(
    [string[]] $PackageArguments = @()
)

$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
$previousFlags = [Environment]::GetEnvironmentVariable('RUSTFLAGS')
try {
    $version = rustc +stable -vV
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $target = ($version | Select-String '^host: ').Line.Substring(6)

    $metadata = cargo +stable metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $targetDirectory = ($metadata | ConvertFrom-Json).target_directory
    $binDir = Join-Path $targetDirectory "$target/release"

    $env:RUSTFLAGS = "$previousFlags -C target-feature=+crt-static".Trim()
    cargo +stable build --locked --workspace --release --target $target
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & (Join-Path $binDir 'kortty-package.exe') --target $target --bin-dir $binDir @PackageArguments
    exit $LASTEXITCODE
} finally {
    [Environment]::SetEnvironmentVariable('RUSTFLAGS', $previousFlags)
    Pop-Location
}
