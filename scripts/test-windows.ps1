param(
    [string[]] $CargoArguments = @('--workspace', '--all-targets')
)

$ErrorActionPreference = 'Stop'
$scratchRoot = Get-Item -LiteralPath ([IO.Path]::GetTempPath())
$scratchPath = Join-Path $scratchRoot.FullName ('kortty-tests-' + [Guid]::NewGuid().ToString('N'))
$scratch = New-Item -ItemType Directory -Path $scratchPath
$previous = @{}
foreach ($name in @('SHELL', 'TEMP', 'TMP', 'LOCALAPPDATA', 'APPDATA')) {
    $previous[$name] = [Environment]::GetEnvironmentVariable($name)
}

Push-Location (Join-Path $PSScriptRoot '..')
try {
    $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $access = [Security.AccessControl.DirectorySecurity]::new()
    $access.SetOwner($owner)
    $access.SetAccessRuleProtection($true, $false)
    $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
        [Security.AccessControl.InheritanceFlags]::ObjectInherit
    $rule = [Security.AccessControl.FileSystemAccessRule]::new(
        $owner,
        [Security.AccessControl.FileSystemRights]::FullControl,
        $inheritance,
        [Security.AccessControl.PropagationFlags]::None,
        [Security.AccessControl.AccessControlType]::Allow
    )
    $access.SetAccessRule($rule)
    Set-Acl -LiteralPath $scratch.FullName -AclObject $access

    $env:SHELL = '/bin/sh'
    $env:TEMP = $scratch.FullName
    $env:TMP = $scratch.FullName
    $env:LOCALAPPDATA = $scratch.FullName
    $env:APPDATA = Join-Path $scratch.FullName 'roaming'
    New-Item -ItemType Directory -Path $env:APPDATA | Out-Null

    cargo +stable test --locked @CargoArguments
    exit $LASTEXITCODE
} finally {
    foreach ($name in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($name, $previous[$name])
    }
    Pop-Location
    $scratch = Get-Item -LiteralPath $scratchPath
    if ($scratch.Parent.FullName.TrimEnd('\', '/') -ne $scratchRoot.FullName.TrimEnd('\', '/') -or
        ($scratch.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Unexpected test scratch directory: $scratchPath"
    }
    Remove-Item -LiteralPath $scratch.FullName -Recurse -Force
}
