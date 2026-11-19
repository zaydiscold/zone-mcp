# Installs zone, the computer side of the Zone iPhone app, on Windows.
#
#   irm https://raw.githubusercontent.com/zaydiscold/zone-mcp/main/install.ps1 | iex
#
# It downloads the latest release from GitHub, checks it against the release's SHA256SUMS,
# puts zone.exe (and zonew.exe, which starts it at login with no window) in
# %LOCALAPPDATA%\Zone\bin and adds that folder to your PATH. No admin rights. Run it again to
# update. `zone update` runs this same script. Works in Windows PowerShell 5.1 and in
# PowerShell 7, on 64-bit Intel and AMD processors and on ARM (Windows 11 runs the Intel
# build there when a release has no ARM one).
#
# Settings, all optional (environment variables):
#   ZONE_VERSION              install this version instead of the latest (for example 0.2.0)
#   ZONE_INSTALL_DIR          put zone.exe here instead of %LOCALAPPDATA%\Zone\bin
#   ZONE_INSTALL_BASE_URL     download from here instead of github.com/zaydiscold/zone-mcp/releases
#   ZONE_INSTALL_NO_RESTART   set to 1 to leave a running zone alone
#   ZONE_INSTALL_NO_PATH      set to 1 to leave your PATH alone
#   ZONE_INSTALL_ARCH         AMD64 or ARM64: install the build for this processor instead

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

    # A proxy that wants the person's Windows sign-in (most office proxies) gets it. The proxy
    # itself is the one Windows is set to use, which both PowerShells follow already.
    try { [Net.WebRequest]::DefaultWebProxy.Credentials = [Net.CredentialCache]::DefaultNetworkCredentials } catch { }
    try { [System.Net.Http.HttpClient]::DefaultProxy.Credentials = [Net.CredentialCache]::DefaultNetworkCredentials } catch { }

    # The processor, as Windows itself has it: a PowerShell that runs emulated on ARM (an x64
    # PowerShell 7) is told AMD64 by its environment, but not by this registry value.
    $arch = $env:ZONE_INSTALL_ARCH
    if (-not $arch) {
        try { $arch = (Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name PROCESSOR_ARCHITECTURE -ErrorAction Stop).PROCESSOR_ARCHITECTURE } catch { }
    }
    if (-not $arch) { $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE } }
    # The builds to look for, best first. Windows 11 on ARM also runs the Intel one, emulated.
    $platforms = switch ($arch.ToUpperInvariant()) {
        'AMD64' { @('windows-x86_64') }
        'ARM64' { @('windows-aarch64', 'windows-x86_64') }
        default { Fail "there is no ready-made zone for Windows on $arch. Build it from source: https://github.com/$repo#from-source" }
    }

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

    $work = Join-Path ([IO.Path]::GetTempPath()) ("zone-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $sums = Join-Path $work 'SHA256SUMS'
        try { Invoke-WebRequest -Uri "$base/download/v$version/SHA256SUMS" -OutFile $sums -UseBasicParsing } catch { Fail "could not download the checksums of zone $version from $base/download/v$version/SHA256SUMS, so nothing can be checked. Is that a released version?" }

        # The first build this computer can run that the release has, and its checksum.
        $listed = @{}
        foreach ($line in Get-Content -LiteralPath $sums) {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2) { $listed[$parts[1].TrimStart('*')] = $parts[0].ToLowerInvariant() }
        }
        $platform = $null
        foreach ($candidate in $platforms) {
            if ($listed.ContainsKey("zone-$version-$candidate.zip")) { $platform = $candidate; break }
        }
        if (-not $platform) { Fail "the checksums of zone $version do not list zone-$version-$($platforms[0]).zip" }
        if ($platform -ne $platforms[0]) {
            Write-Host "zone $version has no build for $($platforms[0]), so the $platform one is installed: Windows 11 runs it on this processor through emulation."
        }
        $asset = "zone-$version-$platform.zip"
        $expected = $listed[$asset]

        Write-Host "Downloading zone $version for $platform"
        $archive = Join-Path $work $asset
        try { Invoke-WebRequest -Uri "$base/download/v$version/$asset" -OutFile $archive -UseBasicParsing } catch { Fail "could not download $base/download/v$version/$asset" }

        # The download must be the file the release says it is.
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { Fail "the download does not match its checksum (expected $expected, got $actual). Nothing was installed." }

        $unpacked = Join-Path $work 'unpacked'
        Expand-Archive -LiteralPath $archive -DestinationPath $unpacked -Force
        $new = Join-Path $unpacked 'zone.exe'
        if (-not (Test-Path -LiteralPath $new)) { Fail "$asset does not hold zone.exe" }
        # It must run here before it replaces anything.
        $installedVersion = & $new --version 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $installedVersion) { Fail 'the downloaded zone does not run on this computer. Nothing was installed.' }
        # Releases from 0.1.4 on carry zonew.exe, which starts zone at login with no window.
        $newWindowless = Join-Path $unpacked 'zonew.exe'

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $target = Join-Path $dir 'zone.exe'
        $updating = Test-Path -LiteralPath $target

        # What an earlier update could not delete because it was still running.
        Get-ChildItem -LiteralPath $dir -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '^zonew?\.exe\.old-[0-9]+$' } | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

        # Whether zone starts at login from this folder: the login entry names this folder, or
        # (zone 0.1.3 and earlier) names a VBScript that starts the program here.
        $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
        # The tests keep their login entries elsewhere, as zone itself does then.
        if ($env:ZONE_WINDOWS_RUN_KEY) { $runKey = "HKCU:\$($env:ZONE_WINDOWS_RUN_KEY)" }
        $entry = $null
        try { $entry = (Get-ItemProperty -LiteralPath $runKey -Name 'Zone' -ErrorAction Stop).Zone } catch { }
        $startsHere = $false
        if ($entry) {
            $here = $dir.TrimEnd('\') + '\'
            if ($entry.IndexOf($here, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $startsHere = $true }
            elseif ($entry -match '"([^"]+\.vbs)"' -and (Test-Path -LiteralPath $Matches[1])) {
                $script = Get-Content -LiteralPath $Matches[1] -Raw -ErrorAction SilentlyContinue
                if ($script -and $script.IndexOf($target, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $startsHere = $true }
            }
        }

        # A program that is running cannot be written over or deleted on Windows, but it can
        # be renamed. So the old one is moved aside, the new one takes its name, and the old
        # one is deleted if nothing is running from it any more (else by the next update, or by
        # zone itself when it next starts).
        function Install-Program([string]$from, [string]$to) {
            if (Test-Path -LiteralPath $to) {
                $aside = "$to.old-" + [DateTime]::UtcNow.Ticks
                Move-Item -LiteralPath $to -Destination $aside -Force
                try {
                    Copy-Item -LiteralPath $from -Destination $to -Force
                } catch {
                    Move-Item -LiteralPath $aside -Destination $to -Force
                    Fail "cannot write to $dir"
                }
                Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue
            } else {
                Copy-Item -LiteralPath $from -Destination $to -Force
            }
        }
        Install-Program $new $target
        if (Test-Path -LiteralPath $newWindowless) { Install-Program $newWindowless (Join-Path $dir 'zonew.exe') }
        Write-Host "Installed $installedVersion in $target"

        if ($startsHere -and $env:ZONE_INSTALL_NO_RESTART -ne '1') {
            # The new zone brings its login entry up to date (moving off the old VBScript), stops
            # the zone that was running, and starts itself in its place.
            & $target install --only-login-item
            if ($LASTEXITCODE -ne 0) { Write-Host 'zone could not restart itself (see above). `zone start` starts it, `zone doctor` says what is wrong.' }
        } elseif ($updating) {
            Write-Host 'If zone is running (`zone status` says), stop it and start it again to run the new version: `zone stop`, then `zone start`.'
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
