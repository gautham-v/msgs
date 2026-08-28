# A Homebrew formula stub for msgs.
#
# The live copy lives in the tap, github.com/gautham-v/homebrew-tap, as
# `Formula/msgs.rb`; users install with `brew install gautham-v/tap/msgs`.
# This one is kept in step with it. On each `v*` tag
# `.github/workflows/release.yml` attaches `msgs-<version>-macos-universal.tar.gz`
# and its .sha256 to the release; copy the version and checksum into both
# files.
#
# Installing from source works too:
#   brew install --HEAD --build-from-source packaging/msgs.rb
class Msgs < Formula
  desc "Terminal client for iMessage on macOS"
  homepage "https://github.com/gautham-v/msgs"
  license "MIT"
  version "0.1.0"

  # The tagged release: a universal binary, so one bottle covers Apple silicon
  # and Intel. Both fields are placeholders until the first tag is pushed.
  url "https://github.com/gautham-v/msgs/releases/download/v0.1.0/msgs-0.1.0-macos-universal.tar.gz"
  sha256 "b5f1d313deef17f615f02e5c5ac7f306703dcaecb75e13db41b88b87a1f65ff7"

  # macOS 14+, which is what msgs supports. SQLite is compiled in, so there is
  # nothing else to depend on.
  depends_on macos: :sonoma

  # Building from HEAD needs a Rust toolchain; the release tarball is a binary
  # and needs nothing.
  head do
    url "https://github.com/gautham-v/msgs.git", branch: "main"
    depends_on "rust" => :build
  end

  # `imsg` sends the tapbacks msgs cannot send through AppleScript. Everything
  # else works without it.
  def caveats
    <<~EOS
      msgs reads ~/Library/Messages/chat.db, which macOS keeps behind Full Disk
      Access. Grant it to the terminal app you run msgs in:

        System Settings → Privacy & Security → Full Disk Access

      then quit that terminal and open it again — macOS only applies the change
      on a fresh launch. `msgs --check` says whether it worked.

      Reactions are sent through `imsg`, which is optional:

        brew install steipete/tap/imsg
    EOS
  end

  def install
    if build.head?
      system "cargo", "install", *std_cargo_args
    else
      bin.install "msgs"
    end
  end

  test do
    assert_match "msgs", shell_output("#{bin}/msgs --version")
    # `--check` is read-only and prints paths and counts only. It exits 0 even
    # with no database and no Full Disk Access, which is the point of it.
    system bin/"msgs", "--check"
  end
end
