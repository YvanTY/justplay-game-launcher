# Builds the two downloads in dist\: JustPlay-portable.zip and JustPlay-Setup.exe.
# Run from the repository root:  .\packaging\build-release.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# Runs an outside program. Windows PowerShell treats anything it writes to stderr (cargo's
# progress, for one) as an error under 'Stop', so its exit code is what counts.
function Invoke-Tool([string]$what, [scriptblock]$command) {
  $ErrorActionPreference = 'Continue'
  & $command 2>&1 | ForEach-Object { "$_" } | Write-Host
  if ($LASTEXITCODE -ne 0) { throw "$what failed (exit code $LASTEXITCODE)" }
}

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "JustPlay $version"

Invoke-Tool 'cargo build' { cargo build --release }

# The files both downloads contain.
$stage = Join-Path $root 'dist\JustPlay'
Remove-Item (Join-Path $root 'dist') -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage | Out-Null
Copy-Item target\release\game-launcher.exe (Join-Path $stage 'JustPlay.exe')
Copy-Item target\release\SDL3.dll $stage
Copy-Item LICENSE (Join-Path $stage 'LICENSE.txt')
Copy-Item packaging\README.txt $stage

# Notices for what is built into JustPlay.exe and shipped beside it.
$notices = New-Object System.Collections.Generic.List[string]
$notices.Add("Third-party software in JustPlay - Game Launcher $version")
$notices.Add("=" * 60)
$notices.Add("")
$notices.Add("Slint (user interface), used under the Slint Royalty-free License 2.0:")
$notices.Add("https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md")
$notices.Add("Made with Slint: https://slint.dev")
$notices.Add("")
$notices.Add("SDL3 (controllers), SDL3.dll, zlib license:")
$notices.Add("")
$notices.AddRange([string[]](Get-Content vendor\SDL3\LICENSE.txt))
$notices.Add("")
$notices.Add("Rust libraries compiled into JustPlay.exe, with their licenses (texts at crates.io):")
$notices.Add("")
$ErrorActionPreference = 'Continue'
$metadata = cargo metadata --format-version 1 --locked 2>$null | ConvertFrom-Json
$ErrorActionPreference = 'Stop'
if (-not $metadata) { throw "cargo metadata failed" }
$metadata.packages |
  Where-Object { $_.name -ne 'game-launcher' } |
  Sort-Object name |
  ForEach-Object { $notices.Add(("  {0} {1}  ({2})" -f $_.name, $_.version, $_.license)) }
Set-Content (Join-Path $stage 'THIRD-PARTY-NOTICES.txt') $notices -Encoding utf8

# Portable download.
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath (Join-Path $root 'dist\JustPlay-portable.zip')

# Installer, with the ViGEmBus driver offered when it is missing.
$vigem = Join-Path $root 'packaging\redist\ViGEmBus_1.22.0_x64_x86_arm64.exe'
if (-not (Test-Path $vigem)) {
  New-Item -ItemType Directory -Force (Split-Path $vigem) | Out-Null
  Invoke-Tool 'ViGEmBus download' {
    gh release download v1.22.0 -R nefarius/ViGEmBus -p 'ViGEmBus_1.22.0_x64_x86_arm64.exe' -D (Split-Path $vigem)
  }
}
$signature = Get-AuthenticodeSignature $vigem
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Nefarius') {
  throw "ViGEmBus installer signature is not valid: $($signature.Status)"
}
$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
  Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 is not installed (winget install JRSoftware.InnoSetup)" }
Invoke-Tool 'Inno Setup' { & $iscc /Q "/DAppVersion=$version" packaging\installer.iss }

Get-ChildItem dist -File | ForEach-Object { "{0}  {1:N1} MB" -f $_.Name, ($_.Length / 1MB) }
