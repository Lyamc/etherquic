# Run by msi-setup.ps1 as a SYSTEM scheduled task. Output: %ProgramData%\etherquic\membrane-install.log
& (Join-Path $PSScriptRoot 'install-membrane-ndis.ps1') *> (Join-Path $env:ProgramData 'etherquic\membrane-install.log')
exit $LASTEXITCODE