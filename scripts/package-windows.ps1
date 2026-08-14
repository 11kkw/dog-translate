[CmdletBinding()]
param(
    [switch]$SkipPortableExe
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$distRoot = Join-Path $projectRoot 'dist'
$cargoManifest = Join-Path $projectRoot 'Cargo.toml'
$cargoText = Get-Content -Raw -LiteralPath $cargoManifest
$versionMatch = [regex]::Match($cargoText, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) {
    throw 'Cargo.toml에서 앱 버전을 찾지 못했습니다.'
}
$version = $versionMatch.Groups[1].Value
$bundleName = "DogTranslate-$version-Windows-x64"
$bundleRoot = Join-Path $distRoot $bundleName
$releaseExe = Join-Path $projectRoot 'target\release\dog-translate.exe'

Push-Location $projectRoot
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release 실패: $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

$codexCandidates = [System.Collections.Generic.List[string]]::new()
if ($env:DOG_TRANSLATE_CODEX_PATH) {
    $codexCandidates.Add($env:DOG_TRANSLATE_CODEX_PATH)
} elseif ($env:DOF_TRANSLATE_CODEX_PATH) {
    $codexCandidates.Add($env:DOF_TRANSLATE_CODEX_PATH)
}
if ($env:APPDATA) {
    $npmCodexRoot = Join-Path $env:APPDATA 'npm\node_modules\@openai\codex\node_modules'
    if (Test-Path -LiteralPath $npmCodexRoot) {
        Get-ChildItem -LiteralPath $npmCodexRoot -Directory -Filter 'codex-win32-*' |
            ForEach-Object {
                Get-ChildItem -LiteralPath $_.FullName -Recurse -File -Filter 'codex.exe' |
                    Where-Object { $_.FullName -match '[\\/]bin[\\/]codex\.exe$' } |
                    ForEach-Object { $codexCandidates.Add($_.FullName) }
            }
    }
}
$codexCommand = Get-Command codex.exe -ErrorAction SilentlyContinue
if ($codexCommand) {
    $codexCandidates.Add($codexCommand.Source)
}
$codexExe = $codexCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $codexExe) {
    throw '네이티브 codex.exe를 찾지 못했습니다. Codex CLI를 설치하거나 DOG_TRANSLATE_CODEX_PATH를 설정하세요.'
}

New-Item -ItemType Directory -Force -Path $distRoot | Out-Null
if (Test-Path -LiteralPath $bundleRoot) {
    $resolvedBundle = [System.IO.Path]::GetFullPath($bundleRoot)
    $resolvedDist = [System.IO.Path]::GetFullPath($distRoot) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $resolvedBundle.StartsWith($resolvedDist, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "배포 폴더가 dist 밖을 가리킵니다: $resolvedBundle"
    }
    Remove-Item -LiteralPath $resolvedBundle -Recurse -Force
}

$providersRoot = Join-Path $bundleRoot 'providers'
New-Item -ItemType Directory -Force -Path $providersRoot | Out-Null
Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $bundleRoot 'DogTranslate.exe')
Copy-Item -LiteralPath $codexExe -Destination (Join-Path $providersRoot 'codex.exe')
Copy-Item -LiteralPath (Join-Path $projectRoot 'packaging\README.txt') -Destination $bundleRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination (Join-Path $bundleRoot 'LICENSE.txt')
Copy-Item -LiteralPath (Join-Path $projectRoot 'NOTICE') -Destination $bundleRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'THIRD_PARTY_LICENSES.html') -Destination $bundleRoot

$zipPath = Join-Path $distRoot "$bundleName.zip"
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}
Compress-Archive -LiteralPath $bundleRoot -DestinationPath $zipPath -CompressionLevel Optimal

if (-not $SkipPortableExe) {
    $iexpress = Join-Path $env:SystemRoot 'System32\iexpress.exe'
    if (-not (Test-Path -LiteralPath $iexpress)) {
        throw 'Windows IExpress를 찾지 못했습니다. -SkipPortableExe로 ZIP만 만들 수 있습니다.'
    }
    $stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("dog-translate-package-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stagingRoot | Out-Null
    try {
        Copy-Item -LiteralPath (Join-Path $bundleRoot 'DogTranslate.exe') -Destination $stagingRoot
        Copy-Item -LiteralPath (Join-Path $providersRoot 'codex.exe') -Destination $stagingRoot
        Copy-Item -LiteralPath (Join-Path $bundleRoot 'README.txt') -Destination $stagingRoot
        Copy-Item -LiteralPath (Join-Path $bundleRoot 'LICENSE.txt') -Destination $stagingRoot
        Copy-Item -LiteralPath (Join-Path $bundleRoot 'NOTICE') -Destination $stagingRoot
        Copy-Item -LiteralPath (Join-Path $bundleRoot 'THIRD_PARTY_LICENSES.html') -Destination $stagingRoot

        $portablePath = Join-Path $distRoot "$bundleName-Portable.exe"
        if (Test-Path -LiteralPath $portablePath) {
            Remove-Item -LiteralPath $portablePath -Force
        }
        $sedPath = Join-Path $stagingRoot 'package.sed'
        $sourcePath = $stagingRoot.TrimEnd('\') + '\'
        $sed = @"
[Version]
Class=IEXPRESS
SEDVersion=3
[Options]
PackagePurpose=InstallApp
ShowInstallProgramWindow=0
HideExtractAnimation=1
UseLongFileName=1
InsideCompressed=0
CAB_FixedSize=0
CAB_ResvCodeSigning=0
RebootMode=N
InstallPrompt=
DisplayLicense=
FinishMessage=
TargetName=$portablePath
FriendlyName=Dog Translate $version Portable
AppLaunched=DogTranslate.exe
PostInstallCmd=<None>
AdminQuietInstCmd=
UserQuietInstCmd=
SourceFiles=SourceFiles
[Strings]
FILE0="DogTranslate.exe"
FILE1="codex.exe"
FILE2="README.txt"
FILE3="LICENSE.txt"
FILE4="NOTICE"
FILE5="THIRD_PARTY_LICENSES.html"
[SourceFiles]
SourceFiles0=$sourcePath
[SourceFiles0]
%FILE0%=
%FILE1%=
%FILE2%=
%FILE3%=
%FILE4%=
%FILE5%=
"@
        Set-Content -LiteralPath $sedPath -Value $sed -Encoding Ascii
        $iexpressProcess = Start-Process -FilePath $iexpress `
            -ArgumentList @('/N', '/Q', $sedPath) `
            -Wait -PassThru -WindowStyle Hidden
        if ($iexpressProcess.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $portablePath)) {
            throw "IExpress 휴대용 실행 파일 생성 실패: $($iexpressProcess.ExitCode)"
        }
    } finally {
        if (Test-Path -LiteralPath $stagingRoot) {
            Remove-Item -LiteralPath $stagingRoot -Recurse -Force
        }
    }
}

Write-Host "배포 폴더: $bundleRoot"
Write-Host "ZIP: $zipPath"
if (-not $SkipPortableExe) {
    Write-Host "단일 실행 파일: $(Join-Path $distRoot "$bundleName-Portable.exe")"
}
