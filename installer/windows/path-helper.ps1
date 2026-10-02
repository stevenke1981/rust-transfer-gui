<#
  Adds or removes a directory from the *system* PATH (HKLM) without duplicates.
  Used by the Rust Transfer GUI installer/uninstaller (run elevated).

  * Reads the raw registry value (DoNotExpandEnvironmentNames) so entries such as
    %SystemRoot%\system32 are preserved verbatim, and keeps the value kind
    (REG_EXPAND_SZ).
  * Comparison is case-insensitive, ignores trailing backslashes and expands
    environment variables, so the directory is never added twice.
  * Refuses to write if the current PATH cannot be read (never truncates PATH).
  The installer broadcasts WM_SETTINGCHANGE("Environment") afterwards.
#>
param(
    [Parameter(Mandatory = $true)][ValidateSet('Add', 'Remove')][string]$Action,
    [Parameter(Mandatory = $true)][string]$Dir
)
$ErrorActionPreference = 'Stop'
$key = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'

$reg = Get-Item -LiteralPath $key
$current = $reg.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
if ([string]::IsNullOrWhiteSpace($current)) {
    Write-Error 'System PATH could not be read; refusing to modify it.'
    exit 1
}
try { $kind = $reg.GetValueKind('Path') } catch { $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString }

function Normalize([string]$p) {
    return [Environment]::ExpandEnvironmentVariables($p.Trim()).TrimEnd('\').ToLowerInvariant()
}

$dirClean = $Dir.Trim().TrimEnd('\')
$target = Normalize $dirClean
$entries = @($current -split ';' | Where-Object { $_.Trim() -ne '' })
$present = @($entries | Where-Object { (Normalize $_) -eq $target }).Count -gt 0

if ($Action -eq 'Add') {
    if ($present) { Write-Output "Already in PATH: $dirClean"; exit 0 }
    $new = ($entries + $dirClean) -join ';'
} else {
    if (-not $present) { Write-Output "Not in PATH: $dirClean"; exit 0 }
    $new = (@($entries | Where-Object { (Normalize $_) -ne $target })) -join ';'
}

Set-ItemProperty -LiteralPath $key -Name 'Path' -Value $new -Type $kind
Write-Output "$Action PATH entry: $dirClean"
exit 0
