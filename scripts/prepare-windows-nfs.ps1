# Only used on an ephemeral administrator-owned GitHub Windows runner.
# A reboot requirement is a failed prerequisite, never an implicit success.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') {
    throw 'Run this feature-setup script only on a disposable GitHub Actions runner.'
}
Write-Host "ImageOS=$env:ImageOS ImageVersion=$env:ImageVersion"
Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber

$feature = Get-WindowsFeature -Name NFS-Client
if ($null -eq $feature) {
    throw 'This Windows image does not provide the NFS-Client feature.'
}
if (-not $feature.Installed) {
    $installation = Install-WindowsFeature -Name NFS-Client -IncludeManagementTools -Restart:$false
    $installation | Format-List Success, RestartNeeded, ExitCode, FeatureResult
    if (-not $installation.Success) {
        throw 'NFS-Client installation failed.'
    }
    if ([string]$installation.RestartNeeded -ne 'No') {
        throw 'NFS-Client requires a reboot; this runner cannot prove native NFS acceptance.'
    }
}

$system32 = Join-Path $env:SystemRoot 'System32'
foreach ($tool in 'mount.exe', 'umount.exe', 'nfsadmin.exe') {
    if (-not (Test-Path (Join-Path $system32 $tool))) {
        throw "Required Windows NFS tool is missing: $tool"
    }
}
& (Join-Path $system32 'nfsadmin.exe') client start
if ($LASTEXITCODE -ne 0 -and (Get-Service NfsClnt).Status -ne 'Running') {
    throw 'The NFS client could not start.'
}

$occupied = [System.Environment]::GetLogicalDrives()
$drive = $null
foreach ($code in 90..80) {
    $candidate = ([char]$code).ToString() + ':'
    if ($occupied -notcontains ($candidate + '\')) {
        $drive = $candidate
        break
    }
}
if ($null -eq $drive) {
    throw 'No unused test drive is available; existing drives will not be touched.'
}
"R2_NFS_TEST_DRIVE=$drive" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
Write-Host "Reserved unused test drive $drive for the isolated native NFS test."
