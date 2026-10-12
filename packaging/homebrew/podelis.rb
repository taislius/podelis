# Homebrew formula template for the tap (github.com/taislius/homebrew-tap,
# as Formula/podelis.rb). scripts/homebrew-formula.sh fills in VERSION (in
# the URLs, e.g. 0.3.0) and the SHA256 values from the release's .sha256
# files; the release workflow does it and pushes the result to the tap.
class Podelis < Formula
  desc "Terminal UI SQL client for PostgreSQL, MySQL and SQLite"
  homepage "https://github.com/taislius/podelis"
  license "AGPL-3.0-only"

  on_macos do
    on_arm do
      url "https://github.com/taislius/podelis/releases/download/vVERSION/podelis-vVERSION-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/taislius/podelis/releases/download/vVERSION/podelis-vVERSION-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/taislius/podelis/releases/download/vVERSION/podelis-vVERSION-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "SHA256_AARCH64_UNKNOWN_LINUX_GNU"
    end
    on_intel do
      url "https://github.com/taislius/podelis/releases/download/vVERSION/podelis-vVERSION-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "SHA256_X86_64_UNKNOWN_LINUX_GNU"
    end
  end

  def install
    bin.install "podelis"
    man1.install "podelis.1"
  end

  test do
    assert_match "podelis #{version}", shell_output("#{bin}/podelis --version")
  end
end
