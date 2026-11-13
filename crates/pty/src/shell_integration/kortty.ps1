# kortty shell integration for Windows PowerShell 5.1 and PowerShell 7.
# Dot-source this file after your existing profile configuration. Local automatic
# integration loads this in the new shell only and never changes a profile file.
if (-not (Get-Variable -Name __kortty_si_loaded -Scope Global -ValueOnly -ErrorAction SilentlyContinue) -and $env:TERM -ne 'dumb') {
    $global:__kortty_si_loaded = $true
    $global:__kortty_si_prompt = $function:prompt
    $global:__kortty_si_executing = $false

    function global:__kortty_si_command_ready([string]$line) {
        if ([string]::IsNullOrWhiteSpace($line)) { return $false }
        $tokens = $null
        $parseErrors = $null
        $null = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$tokens, [ref]$parseErrors)
        return $parseErrors.Count -eq 0
    }

    function global:prompt {
        $succeeded = $?
        $nativeExitVariable = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
        $nativeExit = if ($null -ne $nativeExitVariable) { $nativeExitVariable.Value } else { $null }
        $escape = [char]27
        if ($global:__kortty_si_executing) {
            $code = if ($succeeded) { 0 } else { 1 }
            [Console]::Write("${escape}]133;D;$code`a")
            $global:__kortty_si_executing = $false
        }
        if ($PWD.Provider.Name -eq 'FileSystem') {
            $uri = [Uri]$PWD.ProviderPath
            if ($uri.IsAbsoluteUri -and $uri.IsFile) {
                [Console]::Write("${escape}]7;$($uri.AbsoluteUri)`a")
            }
        }
        [Console]::Write("${escape}]133;A`a")
        if ($null -ne $nativeExitVariable) { $global:LASTEXITCODE = $nativeExit }
        $text = if ($global:__kortty_si_prompt) { & $global:__kortty_si_prompt } else { 'PS> ' }
        if ($null -ne $nativeExitVariable) { $global:LASTEXITCODE = $nativeExit }
        return "$text${escape}]133;B`a"
    }

    # Preserve custom Enter bindings. Without PSReadLine, only prompt boundaries
    # are emitted. kortty invalidates a prompt as soon as ordinary input arrives.
    if (Get-Module PSReadLine) {
        $enter = Get-PSReadLineKeyHandler -Bound | Where-Object { $_.Key -eq 'Enter' }
        if ($null -ne $enter -and $enter.Function -eq 'AcceptLine') {
            Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
                $line = ''
                $cursor = 0
                [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
                if (__kortty_si_command_ready $line) {
                    [Console]::Write("$([char]27)]133;C`a")
                    $global:__kortty_si_executing = $true
                }
                [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
            }
        }
    }
}
