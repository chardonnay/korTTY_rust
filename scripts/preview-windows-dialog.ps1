param(
    [Parameter(Mandatory)]
    [ValidateSet('settings', 'connections', 'ai-manager', 'agent-design', 'agent-design-compact')]
    [string] $Dialog,
    [ValidateRange(30, 3600)]
    [int] $LifetimeSeconds = 600
)

$ErrorActionPreference = 'Stop'
$desktop = Get-Item -LiteralPath (Join-Path $PSScriptRoot '../target/debug/kortty.exe')
$tempDirectory = Get-Item -LiteralPath ([IO.Path]::GetTempPath())
$scratch = Join-Path $tempDirectory.FullName ('kortty-dialog-preview-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $scratch | Out-Null
$environment = @{
    APPDATA = Join-Path $scratch 'roaming'
    LOCALAPPDATA = Join-Path $scratch 'local'
    TEMP = Join-Path $scratch 'temp'
    TMP = Join-Path $scratch 'temp'
    KORTTY_DIALOG_PREVIEW = $Dialog
}
$previous = @{}
$process = $null
try {
    foreach ($key in $environment.Keys) {
        $previous[$key] = [Environment]::GetEnvironmentVariable($key)
    }
    foreach ($folder in @('roaming', 'local', 'temp')) {
        New-Item -ItemType Directory -Path (Join-Path $scratch $folder) | Out-Null
    }
    $profile = New-Item -ItemType Directory -Path (Join-Path $environment.LOCALAPPDATA 'kortty')
    '{"appearance":{"design":"normal"}}' |
        Set-Content -LiteralPath (Join-Path $profile.FullName 'window-settings.json') -Encoding utf8
    foreach ($key in $environment.Keys) {
        [Environment]::SetEnvironmentVariable($key, $environment[$key])
    }
    $process = Start-Process -FilePath $desktop.FullName -WorkingDirectory $scratch -PassThru -WindowStyle Normal
    Write-Output ("Preview process {0} for {1}" -f $process.Id, $Dialog)
    $deadline = [DateTime]::UtcNow.AddSeconds($LifetimeSeconds)
    while (-not $process.WaitForExit(500) -and [DateTime]::UtcNow -lt $deadline) {}
} finally {
    try {
        if ($process -and -not $process.HasExited) {
            $process.Kill($true)
            $process.WaitForExit()
        }
    } finally {
        foreach ($key in $previous.Keys) {
            [Environment]::SetEnvironmentVariable($key, $previous[$key])
        }
        $directory = Get-Item -LiteralPath $scratch
        if ($directory.Parent.FullName.TrimEnd('\', '/') -ne $tempDirectory.FullName.TrimEnd('\', '/') -or
            ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Unexpected preview directory $scratch"
        }
        Remove-Item -LiteralPath $directory.FullName -Recurse -Force
    }
}
