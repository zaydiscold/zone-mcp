# typed: strict
# frozen_string_literal: true

# Homebrew formula for zone, the computer side of the Zone iPhone app.
#
# It installs the program a release was built into, the same archives install.sh downloads.
# Nothing is compiled on the person's computer.
#
# This file is the template. Each release fills in its version and checksums and attaches the
# result to the release as zone.rb (see .github/workflows/release.yml), so the lines marked
# with a platform name below are replaced by a script: keep those markers.
#
# To offer it through Homebrew, make a tap: a repository named zaydiscold/homebrew-zone with
# that release's zone.rb at Formula/zone.rb. Then anybody can run:
#
#   brew install zaydiscold/zone/zone
#
# and `brew upgrade zone` follows new releases once the tap's copy is replaced with the next
# release's zone.rb.
class Zone < Formula
  desc "Your iPhone's Zone dictations on this computer, for you and your AI agents"
  homepage "https://github.com/zaydiscold/zone-mcp"
  version "0.1.0"
  license "MIT"

  # One program for both kinds of Mac, so the two blocks name the same archive.
  on_macos do
    on_arm do
      url "https://github.com/zaydiscold/zone-mcp/releases/download/v#{version}/zone-#{version}-macos-universal.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # macos-universal
    end
    on_intel do
      url "https://github.com/zaydiscold/zone-mcp/releases/download/v#{version}/zone-#{version}-macos-universal.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # macos-universal
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/zaydiscold/zone-mcp/releases/download/v#{version}/zone-#{version}-linux-x86_64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # linux-x86_64
    end
    on_arm do
      url "https://github.com/zaydiscold/zone-mcp/releases/download/v#{version}/zone-#{version}-linux-aarch64.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # linux-aarch64
    end
  end

  def install
    bin.install "zone"
  end

  def caveats
    <<~EOS
      Next:
        zone install    start zone every time you log in, and let your AI agents use it
        zone pair       show the code to scan with Zone on your iPhone
        zone open       see your transcriptions in the browser

      After `brew upgrade zone`, run `zone install` again so the running zone is the new one.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/zone --version")
    assert_match "healthy", shell_output("ZONE_HOME=#{testpath} ZONE_NO_UPDATE_CHECK=1 #{bin}/zone doctor --json", 1)
  end
end
