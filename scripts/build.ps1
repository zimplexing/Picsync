param([switch]$SkipChecks)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Cargo([string[]]$CargoArguments) {
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $($CargoArguments -join ' ') failed ($LASTEXITCODE)" }
}

if (-not $SkipChecks) {
    Invoke-Cargo @('fmt', '--all', '--', '--check')
    Invoke-Cargo @('test', '--locked')
    Invoke-Cargo @('clippy', '--locked', '--all-targets', '--', '-D', 'warnings')
}
Invoke-Cargo @('build', '--release', '--locked')

$metadataText = & cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
$metadata = ($metadataText -join "`n") | ConvertFrom-Json
$rootPackage = $metadata.packages | Where-Object { $_.id -eq $metadata.resolve.root }
if (-not $rootPackage) { throw 'Root package not found' }
$root = (Get-Location).Path
$package = Join-Path $root "dist\Picsync-$($rootPackage.version)-windows-x64"
New-Item -ItemType Directory -Force $package | Out-Null
Copy-Item 'target\release\picsync.exe' (Join-Path $package 'picsync.exe') -Force
Copy-Item 'README.md' (Join-Path $package '使用说明.md') -Force
Copy-Item 'LICENSE' (Join-Path $package 'LICENSE.txt') -Force
Copy-Item 'assets\OFL.txt' (Join-Path $package 'OFL.txt') -Force
if (Test-Path 'docs\screenshot.png') {
    New-Item -ItemType Directory -Force (Join-Path $package 'docs') | Out-Null
    Copy-Item 'docs\screenshot.png' (Join-Path $package 'docs\screenshot.png') -Force
}

# Bundle license texts for every resolved Windows dependency (a conservative
# superset is acceptable). This also finds the embedded default fonts' licenses.
$resolvedIds = @{}
foreach ($entry in $metadata.resolve.nodes) { $resolvedIds[$entry.id] = $true }
$dependencies = $metadata.packages | Where-Object {
    $_.id -ne $rootPackage.id -and $resolvedIds.ContainsKey($_.id)
}
# Crates are not required to ship a license file. Reuse an identical SPDX text
# from a sibling crate when one is available, and record the SPDX id otherwise.
$textByLicense = @{}
$entries = @()
foreach ($dep in ($dependencies | Sort-Object name, version)) {
    $cratePath = Split-Path $dep.manifest_path -Parent
    $licenseFiles = Get-ChildItem $cratePath -File -Recurse | Where-Object {
        $_.Name -match '^(LICENSE|COPYING|NOTICE|OFL)([.\-_]|$)' -or $_.Name -match 'license.*\.txt$'
    }
    $text = [System.Text.StringBuilder]::new()
    foreach ($file in $licenseFiles) {
        if ($file.Length -gt 2000000) { throw "Unexpected license size: $($file.FullName)" }
        [void]$text.AppendLine("--- $($file.FullName.Substring($cratePath.Length + 1)) ---")
        [void]$text.AppendLine([System.IO.File]::ReadAllText($file.FullName))
    }
    if ($text.Length -gt 0 -and $dep.license -and -not $textByLicense.ContainsKey($dep.license)) {
        $textByLicense[$dep.license] = $text.ToString()
    }
    $entries += [pscustomobject]@{ Dep = $dep; Text = $text.ToString() }
}
$unresolved = @()
$notices = [System.Text.StringBuilder]::new()
[void]$notices.AppendLine('Picsync third-party notices. Application source: MIT. Noto Sans SC: see OFL.txt.')
foreach ($entry in $entries) {
    $dep = $entry.Dep
    [void]$notices.AppendLine("`n============================================================")
    [void]$notices.AppendLine("$($dep.name) $($dep.version) | $($dep.license) | $($dep.repository)")
    $text = $entry.Text
    if (-not $text) {
        if ($dep.license -and $textByLicense.ContainsKey($dep.license)) {
            $text = "--- license text reused from another $($dep.license) dependency ---`n" + $textByLicense[$dep.license]
        } else {
            $unresolved += "$($dep.name) $($dep.version) [$($dep.license)]"
            $text = "该依赖的 crate 未附带许可文本；请依据上面的 SPDX 标识与仓库地址核对：$($dep.repository)"
        }
    }
    [void]$notices.AppendLine($text)
}
if ($unresolved.Count -gt 0) {
    Write-Warning "以下依赖需人工核对许可文本：`n$($unresolved -join "`n")"
    [void]$notices.AppendLine("`n`n需要人工核对许可的依赖：`n" + ($unresolved -join "`n"))
}
[System.IO.File]::WriteAllText((Join-Path $package 'THIRD_PARTY_NOTICES.txt'), $notices.ToString(), [System.Text.UTF8Encoding]::new($false))
Get-FileHash (Join-Path $package 'picsync.exe') -Algorithm SHA256 | ForEach-Object {
    "$($_.Hash.ToLower())  picsync.exe" | Set-Content (Join-Path $package 'SHA256SUMS.txt') -Encoding utf8
}
$archive = "$package.zip"
Compress-Archive -Path "$package\*" -DestinationPath $archive -Force
Write-Output "Package: $archive"
