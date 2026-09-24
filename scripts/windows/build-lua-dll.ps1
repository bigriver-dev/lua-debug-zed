<#
.SYNOPSIS
    build Windows DLL (luaXX.dll + luaXX.lib) from lua.org and points mlua at it via env vars.

.PARAMETER LuaVersion
    Short version: 51, 52, 53, 54 or 55.

.PARAMETER OutDir
    Directory to place the built luaXX.dll / luaXX.lib in. Created if missing.

.EXAMPLE
    ./scripts/windows/build-lua-dll.ps1 -LuaVersion 53 -OutDir dist/lua53
#>
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("51", "52", "53", "54", "55")]
    [string]$LuaVersion,

    [Parameter(Mandatory = $true)]
    [string]$OutDir
)

$ErrorActionPreference = "Stop"

$FullVersions = @{
    "51" = "5.1.5"
    "52" = "5.2.4"
    "53" = "5.3.6"
    "54" = "5.4.8"
    "55" = "5.5.0"
}
$FullVersion = $FullVersions[$LuaVersion]
$LibName = "lua$LuaVersion"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$CacheDir = Join-Path $RepoRoot ".lua-src-cache\lua-$FullVersion"
$OutDir = New-Item -ItemType Directory -Force -Path $OutDir | Select-Object -ExpandProperty FullName

# get from official source
if (-not (Test-Path (Join-Path $CacheDir "src"))) {
    Write-Host "Fetching Lua $FullVersion source..."
    $tarballDir = New-Item -ItemType Directory -Force -Path (Join-Path $RepoRoot ".lua-src-cache") | Select-Object -ExpandProperty FullName
    $tarball = Join-Path $tarballDir "lua-$FullVersion.tar.gz"
    if (-not (Test-Path $tarball)) {
        Invoke-WebRequest -Uri "https://www.lua.org/ftp/lua-$FullVersion.tar.gz" -OutFile $tarball
    }
    & "$env:SystemRoot\System32\tar.exe" -xzf $tarball -C $tarballDir
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract lua-$FullVersion.tar.gz" }
}
$SrcDir = Join-Path $CacheDir "src"

# assert MSVC toolchain is on PATH
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    Write-Host "cl.exe not found on PATH -- locating Visual Studio via vswhere..."
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { throw "vswhere.exe not found; install VS Build Tools or run from a Developer shell" }
    $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vsPath) { throw "No Visual Studio installation with the VC++ x86/x64 toolset was found" }
    $vcvars = Join-Path $vsPath "VC\Auxiliary\Build\vcvars64.bat"

    $envOut = cmd.exe /c "`"$vcvars`" && set"
    foreach ($line in $envOut) {
        if ($line -match '^([^=]+)=(.*)$') {
            Set-Item -Path "Env:$($matches[1])" -Value $matches[2] -ErrorAction SilentlyContinue
        }
    }
}

# compile into a .dll
$sources = Get-ChildItem -Path $SrcDir -Filter "*.c" | Where-Object { $_.Name -notin @("lua.c", "luac.c") }
Push-Location $OutDir
try {
    $clArgs = @(
        "/nologo", "/LD", "/O2",
        "/DLUA_BUILD_AS_DLL", "/DLUA_CORE", "/DLUA_LIB", "/D_CRT_SECURE_NO_WARNINGS",
        "/I$SrcDir"
    ) + $sources.FullName + @("/Fe:$LibName.dll", "/link", "/DLL")

    & cl.exe @clArgs
    if ($LASTEXITCODE -ne 0) { throw "cl.exe failed building $LibName.dll (exit $LASTEXITCODE)" }
}
finally {
    Remove-Item "$OutDir\*.obj" -ErrorAction SilentlyContinue
    Pop-Location
}

Write-Host "Built $OutDir\$LibName.dll and $OutDir\$LibName.lib"

# for relative pathing convenience
$env:LUA_LIB = $OutDir
$env:LUA_LIB_NAME = $LibName
$env:LUA_LINK = "dylib"
Write-Host "LUA_LIB=$OutDir"
Write-Host "LUA_LIB_NAME=$LibName"
Write-Host "LUA_LINK=dylib"
