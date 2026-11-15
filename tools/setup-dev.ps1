<#
Sets up a Windows machine to build Arakun and run the checks CI runs.
For contributors and for coding agents. Linux and macOS: tools/setup-dev.sh.

  powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1 [-Yes] [-Check] [-Relay] [-Browser] [-Hooks] [-Build]

  -Yes      do not ask before installing (implied when input is redirected or CI is set)
  -Check    only report what is missing; exit 1 if anything is
  -Relay    also Node 24+ and the npm packages of relay\
  -Browser  also CMake and Ninja, for the `browser` feature (CEF)
  -Hooks    check commit messages locally (git config core.hooksPath tools/hooks);
            the hook is a sh script, so commits need Git for Windows' sh
  -Build    build the tests once, so the first clippy or test run is quick

Always: Git, Visual Studio Build Tools 2022 with the C++ workload (MSVC and a Windows
SDK), and Rust through rustup: stable-x86_64-pc-windows-msvc as the default, and the
version in rust-toolchain.toml with clippy and rustfmt. Installs use winget; Build Tools asks for
administrator rights. Running it again is safe.
See docs/windows.md for what each part is for.
#>
[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$Check,
    [switch]$Relay,
    [switch]$Browser,
    [switch]$Hooks,
    [switch]$Build,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
# Invoke-WebRequest is many times slower with its progress bar.
$ProgressPreference = 'SilentlyContinue'
$Root = Split-Path -Parent $PSScriptRoot

if ($Help) {
    $text = Get-Content $PSCommandPath
    $text[1..([array]::IndexOf($text, '#>') - 1)]
    exit 0
}
if ($env:OS -ne 'Windows_NT') {
    Write-Host 'setup-dev: this is for Windows; on Linux and macOS run tools/setup-dev.sh'
    exit 2
}
# An agent or a CI job cannot answer a prompt.
if ([Console]::IsInputRedirected -or $env:CI) { $Yes = $true }

$script:Missing = $false
function Say([string]$Text) { Write-Host "setup-dev: $Text" }
function Ok([string]$Text) { Write-Host "  ok       $Text" }
function Lack([string]$Text) { Write-Host "  missing  $Text"; $script:Missing = $true }
function Fail([string]$Text) { Write-Host "setup-dev: $Text" -ForegroundColor Red; exit 1 }

function Ask([string]$Question) {
    if ($Yes) { return $true }
    $answer = Read-Host "setup-dev: $Question [Y/n]"
    return -not ($answer -match '^[nN]')
}

# The output of a native command, '' if it fails. Windows PowerShell 5.1 turns a
# redirected stderr line into an error, which 'Stop' would make fatal.
function Quiet([scriptblock]$Block) {
    $ErrorActionPreference = 'Continue'
    try { return (& $Block 2>$null | Out-String).Trim() } catch { return '' }
}

function Has([string]$Command) {
    return [bool](Get-Command $Command -ErrorAction SilentlyContinue)
}

# winget and installers change PATH in the registry only; this shell needs it too. New
# entries are added at the end, so what this shell already has (a Developer PowerShell's
# MSVC paths, say) stays first.
function Import-RegistryPath {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if ($env:CARGO_HOME) { $cargoBin = Join-Path $env:CARGO_HOME 'bin' }
    $have = @($env:Path -split ';' | Where-Object { $_ })
    $more = @([Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';') +
        @([Environment]::GetEnvironmentVariable('Path', 'User') -split ';') + @($cargoBin)
    foreach ($dir in $more) {
        if ($dir -and ($have -notcontains $dir) -and (Test-Path $dir)) { $have += $dir }
    }
    $env:Path = $have -join ';'
}

function Install-Winget([string]$Id, [string[]]$Extra = @()) {
    if (-not (Has 'winget')) {
        Fail "winget is not available; install $Id by hand (or App Installer from the Microsoft Store)"
    }
    $wingetArgs = @('install', '--id', $Id, '--exact', '--source', 'winget',
        '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity') + $Extra
    & winget @wingetArgs
    # 3010: installed, a restart finishes it (Visual Studio's installer says this).
    if ($LASTEXITCODE -eq 3010) { Say "$Id wants a restart of Windows to finish" }
    elseif ($LASTEXITCODE -ne 0) { Fail "winget install $Id failed (exit $LASTEXITCODE)" }
    Import-RegistryPath
}

# The newest Visual Studio (any edition, Build Tools included) with every component asked
# for, or $null.
function Find-VisualStudio([string[]]$Components) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }
    $vswhereArgs = @('-latest', '-products', '*', '-property', 'installationPath')
    if ($Components.Count -gt 0) { $vswhereArgs += @('-requires') + $Components }
    $path = & $vswhere @vswhereArgs | Select-Object -First 1
    if ($path) { return $path.Trim() }
    return $null
}

Import-RegistryPath

# --- Git ---
Say 'Git'
if (Has 'git') {
    Ok (& git --version)
} elseif ($Check) {
    Lack 'Git for Windows (winget install Git.Git)'
} elseif (Ask 'install Git for Windows with winget?') {
    Install-Winget 'Git.Git'
} else {
    Fail 'skipped; Arakun and its tests need git'
}

# --- Visual Studio Build Tools ---
Say 'MSVC and the Windows SDK'
$vcTools = 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64'
$cmakeTools = 'Microsoft.VisualStudio.Component.VC.CMake.Project'
$sdkRoot = $null
try {
    $sdkRoot = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -ErrorAction Stop).KitsRoot10
} catch {
    $sdkRoot = $null # no Windows SDK installed
}

$vs = Find-VisualStudio @($vcTools)
$anyVs = Find-VisualStudio @()
if ($vs -and $sdkRoot) {
    Ok "MSVC in $vs"
    Ok "Windows SDK in $sdkRoot"
} elseif ($Check) {
    if (-not $vs) { Lack 'MSVC (Visual Studio Build Tools 2022, "Desktop development with C++")' }
    if (-not $sdkRoot) { Lack 'a Windows 10/11 SDK' }
} elseif ($anyVs) {
    # winget only installs; adding a workload to an existing Visual Studio is the
    # Visual Studio Installer's job, and the workload's name differs by edition.
    Fail ("$anyVs lacks MSVC or the Windows SDK. Open the Visual Studio Installer, " +
        'choose Modify and add "Desktop development with C++", then run this again')
} elseif (Ask 'install Visual Studio Build Tools 2022 with the C++ workload (several GB, needs administrator rights)?') {
    # includeRecommended brings the Windows SDK, CMake and Ninja with the workload.
    Install-Winget 'Microsoft.VisualStudio.2022.BuildTools' @('--override',
        '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')
    $vs = Find-VisualStudio @($vcTools)
    if (-not $vs) { Fail 'the Build Tools install finished without MSVC; check the Visual Studio Installer' }
    Ok "MSVC in $vs"
} else {
    Fail 'skipped; the build needs MSVC'
}

if ($Browser) {
    Say 'CMake and Ninja (browser)'
    if (Find-VisualStudio @($vcTools, $cmakeTools)) {
        Ok 'C++ CMake tools for Windows (CMake and Ninja in the Developer PowerShell)'
    } else {
        # Same reason as above: a change to an existing install goes through its installer.
        Lack '"C++ CMake tools for Windows" (Visual Studio Installer, Modify, Individual components)'
    }
}

# --- Rust ---
Say 'Rust'
$installedRustup = $false
if (Has 'rustup') {
    # The default, not the active toolchain: in the checkout that is the one in
    # rust-toolchain.toml, and asking for it would install it, even with -Check.
    $active = Quiet { rustup default }
    if (-not $active) {
        if ($Check) { Lack 'a default toolchain (rustup default stable-msvc)' }
        else { & rustup default stable-msvc; if ($LASTEXITCODE -ne 0) { Fail 'rustup default failed' } }
    } elseif ($active -notlike '*msvc*') {
        # The gnu toolchain builds too, but docs/windows.md and the CEF download expect MSVC.
        Say "the default toolchain is $active; Arakun is built with MSVC (rustup default stable-msvc)"
    } else {
        Ok "rustup ($active)"
    }
} elseif (Has 'cargo') {
    Ok "cargo without rustup ($(& cargo --version)); if the build asks for a newer Rust, use rustup"
} elseif ($Check) {
    Lack 'Rust (https://rustup.rs)'
} elseif (Ask 'install Rust (stable-x86_64-pc-windows-msvc) with rustup into ~\.cargo?') {
    $dir = Join-Path ([IO.Path]::GetTempPath()) "arakun-rustup-$PID"
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $init = Join-Path $dir 'rustup-init.exe'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile $init
    & $init -y --profile minimal --default-host x86_64-pc-windows-msvc --default-toolchain stable --component clippy
    $code = $LASTEXITCODE
    Remove-Item -Recurse -Force $dir
    if ($code -ne 0) { Fail "rustup-init failed (exit $code)" }
    Import-RegistryPath
    $installedRustup = $true
} else {
    Fail 'skipped; the build needs Rust'
}

# In the checkout, rustup uses the version and components in rust-toolchain.toml. The
# check must not install it, so rustup's own install on first use is off for it.
if (Has 'cargo') {
    Push-Location $Root
    $autoInstall = $env:RUSTUP_AUTO_INSTALL
    $env:RUSTUP_AUTO_INSTALL = '0'
    $missingComponents = @()
    try {
        foreach ($component in @('clippy', 'rustfmt')) {
            $subcommand = if ($component -eq 'rustfmt') { 'fmt' } else { $component }
            $version = Quiet { cargo $subcommand --version }
            if ($LASTEXITCODE -ne 0) { $version = '' }
            if ($version) {
                Ok $version
            } else {
                $missingComponents += $component
                if ($Check) {
                    Lack "$component of the Rust version in rust-toolchain.toml (rustup toolchain install, in the checkout)"
                }
            }
        }
    }
    finally { $env:RUSTUP_AUTO_INSTALL = $autoInstall; Pop-Location }
    if ($missingComponents.Count -gt 0 -and -not $Check) {
        if (Has 'rustup') {
            Push-Location $Root
            try { & rustup toolchain install --no-self-update } finally { Pop-Location }
            if ($LASTEXITCODE -ne 0) { Fail 'rustup toolchain install failed' }
        } else {
            Fail "missing Rust components; install clippy and rustfmt with your distribution's Rust"
        }
    }
}

# --- relay ---
if ($Relay) {
    Say 'relay (Node 24+)'
    $major = 0
    if (Has 'node') { $major = [int](& node -p 'process.versions.node.split(".")[0]') }
    if ($major -lt 24) {
        if ($Check) {
            Lack 'Node 24 or newer (winget install OpenJS.NodeJS.LTS)'
        } elseif (Ask 'install Node.js LTS with winget?') {
            Install-Winget 'OpenJS.NodeJS.LTS'
            $major = [int](& node -p 'process.versions.node.split(".")[0]')
            if ($major -lt 24) { Fail "winget installed Node $major; the relay needs 24 or newer" }
        } else {
            Fail 'skipped; the relay needs Node 24 or newer'
        }
    }
    if ($major -ge 24) {
        Ok "node $(& node --version)"
        $modules = Join-Path $Root 'relay\node_modules'
        if ($Check) {
            if (Test-Path $modules) { Ok 'relay\node_modules' } else { Lack 'relay\node_modules (npm ci)' }
        } else {
            Push-Location (Join-Path $Root 'relay')
            try {
                & npm ci --no-audit --no-fund
                if ($LASTEXITCODE -ne 0) { Fail 'npm ci failed in relay' }
            } finally { Pop-Location }
        }
    }
}

# --- git hook ---
if ($Hooks) {
    Say 'commit-msg hook'
    $current = Quiet { git -C $Root config core.hooksPath }
    if ($current -eq 'tools/hooks') {
        Ok 'core.hooksPath is tools/hooks'
    } elseif ($Check) {
        Lack 'core.hooksPath (git config core.hooksPath tools/hooks)'
    } else {
        & git -C $Root config core.hooksPath tools/hooks
        Ok 'core.hooksPath set to tools/hooks (undo: git config --unset core.hooksPath)'
    }
}

if ($Check) {
    if ($script:Missing) { Say 'something is missing; run tools\setup-dev.ps1 to install it'; exit 1 }
    Say 'everything is in place'
    exit 0
}
if ($script:Missing) { Fail "done, but see 'missing' above" }
if ($installedRustup) { Say 'open a new terminal so it finds cargo on PATH' }

# --- warm build ---
if ($Build) {
    # cc and the linker need the MSVC environment, which the Developer PowerShell sets up.
    $vs = Find-VisualStudio @($vcTools)
    Import-Module (Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
    Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
    Push-Location $Root
    try {
        if ($Browser) {
            Say 'building with the browser (the first build downloads CEF and takes a while)'
            & cargo test --locked --no-run
        } else {
            Say 'building the tests (the first build takes a while)'
            & cargo test --locked --no-default-features --no-run
        }
        if ($LASTEXITCODE -ne 0) { Fail 'the build failed' }
    } finally { Pop-Location }
}

Say 'ready. In a Developer PowerShell for VS 2022, the checks CI runs:'
Write-Host '  cargo clippy --locked --all-targets --no-default-features -- -D warnings'
Write-Host '  cargo test --locked --no-default-features'
