# Installs zone, the computer side of the Zone iPhone app, on Windows.
#
#   irm https://raw.githubusercontent.com/zaydiscold/zone-mcp/main/install.ps1 | iex
#
# It downloads the latest release from GitHub, checks it against the release's SHA256SUMS,
# puts the one program, zone.exe, in %LOCALAPPDATA%\Zone\bin and adds that folder to your
# PATH. No admin rights. Run it again to update. `zone update` runs this same script.
# Works in Windows PowerShell 5.1 and in PowerShell 7.
#
# Settings, all optional (environment variables):
#   ZONE_VERSION              install this version instead of the latest (for example 0.2.0)
#   ZONE_INSTALL_DIR          put zone.exe here instead of %LOCALAPPDATA%\Zone\bin
#   ZONE_INSTALL_BASE_URL     download from here instead of github.com/zaydiscold/zone-mcp/releases
#   ZONE_INSTALL_NO_RESTART   set to 1 to leave a running zone alone
#   ZONE_INSTALL_NO_PATH      set to 1 to leave your PATH alone

# Everything runs inside one block, so that nothing set here stays behind in the PowerShell
# it was pasted into, and a download cut off halfway runs nothing at all.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $repo = 'zaydiscold/zone-mcp'

    function Fail([string]$message) {
        throw "zone install: $message"
    }

    $base = if ($env:ZONE_INSTALL_BASE_URL) { $env:ZONE_INSTALL_BASE_URL.TrimEnd('/') } else { "https://github.com/$repo/releases" }
    $dir = if ($env:ZONE_INSTALL_DIR) { $env:ZONE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Zone\bin' }
    $version = $env:ZONE_VERSION

    # Windows PowerShell 5.1 does not offer TLS 1.2 unless asked, and GitHub needs it.
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch { }

    # The one ready-made zone for Windows is for 64-bit Intel and AMD processors. Windows on
    # ARM runs it too.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        Fail "there is no ready-made zone for Windows on $arch. Build it from source: https://github.com/$repo#from-source"
    }
    $platform = 'windows-x86_64'

    # Which version is the latest: GitHub sends /releases/latest on to /releases/tag/v<version>.
    if (-not $version) {
        try {
            $response = Invoke-WebRequest -Uri "$base/latest" -UseBasicParsing -MaximumRedirection 10
        } catch {
            Fail "could not reach $base/latest. Is this computer online?"
        }
        $final = $null
        if ($response.BaseResponse.ResponseUri) { $final = $response.BaseResponse.ResponseUri.AbsoluteUri }
        elseif ($response.BaseResponse.RequestMessage) { $final = $response.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
        if (-not $final) { Fail 'could not tell which version is the latest' }
        $version = ($final.TrimEnd('/') -split '/')[-1]
    }
    $version = $version -replace '^[vV]', ''
    if ($version -notmatch '^[0-9]') {
        Fail "could not tell which version is the latest (got `"$version`"). No release may have been published yet: see https://github.com/$repo/releases"
    }

    $asset = "zone-$version-$platform.zip"
    $work = Join-Path ([IO.Path]::GetTempPath()) ("zone-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        Write-Host "Downloading zone $version for $platform"
        $archive = Join-Path $work $asset
        $sums = Join-Path $work 'SHA256SUMS'
        try { Invoke-WebRequest -Uri "$base/download/v$version/$asset" -OutFile $archive -UseBasicParsing } catch { Fail "could not download $base/download/v$version/$asset" }
        try { Invoke-WebRequest -Uri "$base/download/v$version/SHA256SUMS" -OutFile $sums -UseBasicParsing } catch { Fail "could not download the checksums of zone $version, so the download cannot be checked" }

        # The download must be the file the release says it is.
        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $asset) { $expected = $parts[0].ToLowerInvariant() }
        }
        if (-not $expected) { Fail "the checksums of zone $version do not list $asset" }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { Fail "the download does not match its checksum (expected $expected, got $actual). Nothing was installed." }

        $unpacked = Join-Path $work 'unpacked'
        Expand-Archive -LiteralPath $archive -DestinationPath $unpacked -Force
        $new = Join-Path $unpacked 'zone.exe'
        if (-not (Test-Path -LiteralPath $new)) { Fail "$asset does not hold zone.exe" }
        # It must run here before it replaces anything.
        $installedVersion = & $new --version 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $installedVersion) { Fail 'the downloaded zone does not run on this computer. Nothing was installed.' }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $target = Join-Path $dir 'zone.exe'
        $updating = Test-Path -LiteralPath $target

        # What an earlier update could not delete because it was still running.
        Get-ChildItem -LiteralPath $dir -Filter 'zone.exe.old-*' -ErrorAction SilentlyContinue | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

        # The zone that runs in the background from this folder, if there is one.
        $daemons = @()
        if ($updating) {
            $full = (Resolve-Path -LiteralPath $target).Path
            $daemons = @(Get-CimInstance Win32_Process -Filter "Name = 'zone.exe'" -ErrorAction SilentlyContinue | Where-Object { $_.ExecutablePath -eq $full -and $_.CommandLine -match '\sserve(\s|$)' })
        }
        $launcher = Join-Path $env:LOCALAPPDATA 'Zone\zone-serve.vbs'
        if ($env:ZONE_HOME) { $launcher = Join-Path $env:ZONE_HOME 'zone-serve.vbs' }
        $restart = ($daemons.Count -gt 0) -and (Test-Path -LiteralPath $launcher) -and ($env:ZONE_INSTALL_NO_RESTART -ne '1')
        if ($restart) {
            foreach ($daemon in $daemons) { Stop-Process -Id $daemon.ProcessId -Force -ErrorAction SilentlyContinue }
            Start-Sleep -Milliseconds 500
        }

        # A program that is running cannot be written over or deleted on Windows, but it can
        # be renamed. So the old one is moved aside, the new one takes its name, and the old
        # one is deleted if nothing is running from it any more.
        if ($updating) {
            $aside = "$target.old-" + [DateTime]::UtcNow.Ticks
            Move-Item -LiteralPath $target -Destination $aside -Force
            try {
                Copy-Item -LiteralPath $new -Destination $target -Force
            } catch {
                Move-Item -LiteralPath $aside -Destination $target -Force
                Fail "cannot write to $dir"
            }
            Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue
        } else {
            Copy-Item -LiteralPath $new -Destination $target -Force
        }
        Write-Host "Installed $installedVersion in $target"

        if ($restart) {
            Start-Process -FilePath 'wscript.exe' -ArgumentList ('"' + $launcher + '"') | Out-Null
            Write-Host 'Restarted zone, so the new version is the one running.'
        } elseif ($updating) {
            Write-Host 'If zone is running (`zone status` says), stop it and start it again to run the new version.'
        }

        # The folder goes on your PATH, so that `zone` works in every new terminal.
        if ($env:ZONE_INSTALL_NO_PATH -ne '1' -and -not $env:ZONE_INSTALL_UPDATING) {
            $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
            $entries = @()
            if ($userPath) { $entries = @($userPath -split ';' | Where-Object { $_ }) }
            if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) {
                [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
                Write-Host "Added $dir to your PATH. Open a new terminal for ``zone`` to be found."
            }
            if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) { $env:Path = "$env:Path;$dir" }
        }

        if (-not $env:ZONE_INSTALL_UPDATING) {
            Write-Host ''
            Write-Host 'Next:'
            Write-Host '  zone install    start zone every time you log in, and let your AI agents use it'
            Write-Host '  zone pair       show the code to scan with Zone on your iPhone'
            Write-Host '  zone open       see your transcriptions in the browser'
        }
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}
