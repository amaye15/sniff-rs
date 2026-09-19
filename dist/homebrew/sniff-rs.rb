# Draft Homebrew formula for sniff-rs.
#
# This file is NOT used from this repo directly. Copy it to your tap repo
# (e.g. amaye15/homebrew-sniff-rs/Formula/sniff-rs.rb), then for each
# release update `version` and the four `sha256` values below from that
# release's SHA256SUMS.txt asset. Asset names come from
# .github/workflows/release.yml - the two must stay in sync.
class SniffRs < Formula
  desc "Profile a data file and produce a data dictionary"
  homepage "https://github.com/amaye15/sniff-rs"
  version "0.1.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/amaye15/sniff-rs/releases/download/v0.1.0/sniff-rs-aarch64-apple-darwin.tar.gz"
      sha256 "UPDATE_ME_FROM_SHA256SUMS_TXT"
    end
    on_intel do
      url "https://github.com/amaye15/sniff-rs/releases/download/v0.1.0/sniff-rs-x86_64-apple-darwin.tar.gz"
      sha256 "UPDATE_ME_FROM_SHA256SUMS_TXT"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/amaye15/sniff-rs/releases/download/v0.1.0/sniff-rs-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "UPDATE_ME_FROM_SHA256SUMS_TXT"
    end
    on_intel do
      url "https://github.com/amaye15/sniff-rs/releases/download/v0.1.0/sniff-rs-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "UPDATE_ME_FROM_SHA256SUMS_TXT"
    end
  end

  def install
    bin.install Dir["sniff-rs-*/sniff-rs"]
  end

  test do
    system bin/"sniff-rs", "--version"
  end
end
