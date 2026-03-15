class Polyphony < Formula
  desc "Repo-native AI orchestration tool"
  homepage "https://github.com/penso/polyphony"
  url "https://github.com/penso/polyphony/releases/download/TAG_PLACEHOLDER/polyphony-TAG_PLACEHOLDER-universal2-apple-darwin.tar.gz"
  version "TAG_PLACEHOLDER"
  sha256 "SHA256_PLACEHOLDER"
  license "MIT"

  def install
    bin.install "bin/polyphony"
    doc.install "README.md"
    pkgshare.install "LICENSE"
    pkgshare.install "CHANGELOG.md" if File.exist?("CHANGELOG.md")
  end

  test do
    assert_match "polyphony", shell_output("#{bin}/polyphony --help")
  end
end
