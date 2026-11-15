# Installs the latest Arakun release for the current user on Windows.
#
#   irm https://arakun.dev/install.ps1 | iex
#
# Puts arakun.exe in %LOCALAPPDATA%\Programs\Arakun, adds that folder to the user PATH and
# adds a Start menu shortcut. $env:ARAKUN_VERSION = "v0.1.0" pins a release. Release builds
# have no browser tabs; for those, build from source (see docs/windows.md).
#
# Works in Windows PowerShell 5.1 and PowerShell 7. Under `iex`, `exit` would close the
# user's window, so errors throw instead. Everything runs from the call at the end, so a
# download cut short runs nothing.

function Install-Arakun {
    $ErrorActionPreference = 'Stop'
    # Faster downloads: the progress bar slows Invoke-WebRequest down a lot in 5.1.
    $ProgressPreference = 'SilentlyContinue'
    # 5.1 may still default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'arakunhq/arakun'

    # PROCESSOR_ARCHITEW6432 is set in a 32-bit PowerShell on 64-bit Windows.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64') {
        throw "arakun: Windows on $arch has no release build; build from source (see docs/windows.md)"
    }

    $version = $env:ARAKUN_VERSION
    if (-not $version) {
        $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/latest"
        $version = $release.tag_name
        if (-not $version) { throw "arakun: no release found at https://github.com/$repo/releases" }
    } elseif (-not $version.StartsWith('v')) {
        $version = "v$version"
    }

    $name = "arakun-$version-windows-x86_64"
    $base = "https://github.com/$repo/releases/download/$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("arakun-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "arakun: downloading Arakun $version for windows-x86_64"
        $zip = Join-Path $tmp "$name.zip"
        $sums = Join-Path $tmp 'SHA256SUMS'
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$name.zip" -OutFile $zip
        Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums

        $want = $null
        foreach ($line in Get-Content $sums) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq "$name.zip") { $want = $parts[0].ToLower() }
        }
        if (-not $want) { throw "arakun: $name.zip is not listed in SHA256SUMS" }
        $have = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
        if ($have -ne $want) { throw "arakun: checksum mismatch for $name.zip (expected $want, got $have)" }

        $unpacked = [IO.Path]::Combine($tmp, 'x', $name)
        Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp 'x')
        $exe = Join-Path $unpacked 'arakun.exe'
        if (-not (Test-Path $exe)) { throw 'arakun: the archive has no arakun.exe' }

        $dir = [IO.Path]::Combine($env:LOCALAPPDATA, 'Programs', 'Arakun')
        $target = Join-Path $dir 'arakun.exe'
        # Windows keeps a running .exe locked, so it cannot be replaced.
        if (Get-Process -Name arakun -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target }) {
            throw 'arakun: Arakun is running; quit it and run the installer again'
        }

        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        Copy-Item -Force $exe $target
        foreach ($f in 'LICENSE', 'NOTICE', 'README.md') {
            Copy-Item -Force (Join-Path $unpacked $f) $dir -ErrorAction SilentlyContinue
        }

        # The user PATH as stored, not $env:Path, which also holds the machine PATH.
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { $_ })
        if ($entries -notcontains $dir) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
            $env:Path = "$env:Path;$dir"
            Write-Host "arakun: added $dir to your PATH (new terminals see it)"
        }

        $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Arakun.lnk'
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($lnk)
        $shortcut.TargetPath = $target
        $shortcut.Save()

        Write-Host "arakun: installed arakun.exe in $dir"
        Write-Host "arakun: done. Run 'arakun' or open Arakun from the Start menu."
        Write-Host "arakun: to uninstall, delete $dir and $lnk, and remove $dir from your user PATH"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

Install-Arakun
