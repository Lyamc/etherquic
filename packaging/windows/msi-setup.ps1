# Run by the etherquic MSI as LocalSystem (deferred, no impersonation).
# Install: optional Membrane NDIS filter, then the existing etherquic-setup flow.
# Uninstall: etherquic-setup --uninstall. Log: %ProgramData%\etherquic\msi-setup.log
param(
    [string]$Membrane = "1",
    [string]$Access = "",
    [switch]$Uninstall
)
# Continue: PowerShell 5.1 turns a native command's stderr into an error under Stop.
$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
$logDir = Join-Path $env:ProgramData 'etherquic'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$log = Join-Path $logDir 'msi-setup.log'
function Log([string]$m) { Add-Content -Path $log -Value ("{0} {1}" -f (Get-Date -Format s), $m) }
$setup = Join-Path $here 'etherquic-setup.exe'

if ($Uninstall) {
    $out = & $setup --uninstall 2>&1 | Out-String
    Log "etherquic-setup --uninstall exit ${LASTEXITCODE}: $out"
    exit 0
}

$running = (Get-Service membrane_ndis -ErrorAction SilentlyContinue).Status -eq 'Running'
if ($Membrane -eq '1' -and $running) {
    Log "Membrane filter is already running; leaving it as it is (MEMBRANE=force reinstalls)."
} elseif ($Membrane -eq '1' -or $Membrane -eq 'force') {
    # msiexec strips SeSystemEnvironmentPrivilege from a custom action's token, so
    # bcdedit and Confirm-SecureBootUEFI fail in this process (ERROR_NOT_ALL_ASSIGNED).
    # Run the Membrane script as a one-shot SYSTEM task, which has the full token.
    $runner = Join-Path $here 'membrane\run-logged.ps1'
    $ps = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $task = 'etherquic-membrane-install'
    $outLog = Join-Path $logDir 'membrane-install.log'
    Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue
    $action = New-ScheduledTaskAction -Execute $ps -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$runner`""
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Force | Out-Null
    Start-ScheduledTask -TaskName $task
    $deadline = (Get-Date).AddMinutes(5)
    Start-Sleep -Seconds 2
    while ((Get-ScheduledTask -TaskName $task).State -eq 'Running' -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 1 }
    $code = (Get-ScheduledTaskInfo -TaskName $task).LastTaskResult
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
    $out = if (Test-Path $outLog) { Get-Content $outLog -Raw } else { '' }
    Log "install-membrane-ndis exit ${code}: $out"
    if ($code -eq 3010) {
        Log "Membrane: reboot required before the filter can load (test signing was turned on)."
    } elseif ($code -ne 0) {
        # Not fatal: a machine with Npcap does not need Membrane. Secure Boot blocks it.
        Log "Membrane was not installed. The host needs Membrane or Npcap for L2."
    }
}

$flags = @()
switch ($Access) {
    'unattended' { $flags += '--unattended' }
    'attended' { $flags += '--attended' }
}
$flags += @('--payload', $here)
$out = & $setup @flags 2>&1 | Out-String
$code = $LASTEXITCODE
Log "etherquic-setup $($flags -join ' ') exit ${code}: $out"
exit $code