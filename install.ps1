<#
.SYNOPSIS
  Install sniff-rs from GitHub releases (native Windows installer).
.DESCRIPTION
  Downloads the Windows release asset plus SHA256SUMS.txt, verifies the
  checksum, and installs sniff-rs.exe. No third-party tools needed -
  only built-in PowerShell cmdlets.
.EXAMPLE
  powershell -c "irm https://raw.githubusercontent.com/amaye15/sniff-rs/main/install.ps1 | iex"
.EXAMPLE
  .\install.ps1 -Version 0.1.0 -To "$env:USERPROFILE\bin"
#>
param(
  [string]$Version = "latest",
  [string]$To = (Join-Path $env:LocalAppData "sniff-rs\bin")
)

$ErrorActionPreference = "Stop"
# TLS 1.2 for Windows PowerShell 5.1 (7+ already defaults to it).
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = "amaye15/sniff-rs"

if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") {
  throw "unsupported arch: $($env:PROCESSOR_ARCHITECTURE) (only x86_64 Windows builds are published)"
}
$Target = "x86_64-pc-windows-msvc"

if ($Version -eq "latest") {
  # Built-in JSON parsing, so the API is simpler than following redirects.
  $Tag = (Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest").tag_name
  $Version = $Tag.TrimStart("v")
}
$Version = $Version.TrimStart("v")
$Asset = "sniff-rs-$Target.zip"
$Base = if ($env:SNIFF_RS_BASE) { $env:SNIFF_RS_BASE } else { "https://github.com/$Repo/releases/download/v$Version" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("sniff-rs-install-" + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Invoke-WebRequest -Uri "$Base/$Asset" -OutFile (Join-Path $tmp $Asset)
  Invoke-WebRequest -Uri "$Base/SHA256SUMS.txt" -OutFile (Join-Path $tmp "SHA256SUMS.txt")

  # Match the exact asset line (two spaces separate hash and filename).
  # -like without a trailing wildcard is an ends-with match, so this cannot
  # hit a longer filename that merely starts with the asset name.
  $entry = Get-Content (Join-Path $tmp "SHA256SUMS.txt") |
    Where-Object { $_ -like "*  $Asset" } | Select-Object -First 1
  if (-not $entry) { throw "checksum entry for $Asset not found in SHA256SUMS.txt" }
  $expected = $entry.Split(" ")[0]
  $actual = (Get-FileHash -Path (Join-Path $tmp $Asset) -Algorithm SHA256).Hash
  if ($actual -ne $expected) { throw "checksum mismatch for $Asset" }

  Expand-Archive -Path (Join-Path $tmp $Asset) -DestinationPath (Join-Path $tmp "unpacked") -Force
  $exe = Get-ChildItem -Path (Join-Path $tmp "unpacked") -Filter "sniff-rs.exe" -Recurse |
    Select-Object -First 1 -ExpandProperty FullName
  if (-not $exe) { throw "sniff-rs.exe not found inside $Asset" }
  New-Item -ItemType Directory -Path $To -Force | Out-Null
  Copy-Item $exe (Join-Path $To "sniff-rs.exe") -Force
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

& (Join-Path $To "sniff-rs.exe") --version
Write-Output "installed to $(Join-Path $To 'sniff-rs.exe')"
if (($env:PATH -split ";") -notcontains $To) {
  Write-Output "note: $To is not on your PATH - add it to use sniff-rs from any terminal"
}
