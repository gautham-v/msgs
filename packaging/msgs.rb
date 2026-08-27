# A Homebrew formula stub for msgs.
#
# It is a stub because there is no tap and no tagged release yet: the URL and
# the sha256 below are the shape a release fills in, not values that resolve
# today. `.github/workflows/release.yml` builds the universal binary this
# formula would install and prints the checksum to paste in.
#
# To take it live:
#   1. push a `v*` tag, let the release workflow attach
#      `msgs-<version>-macos-universal.tar.gz` and its .sha256
#   2. copy that checksum into `sha256` below and set `version`
#   3. copy this file into a tap repo as `Formula/msgs.rb`
#      (`brew tap-new <you>/tap`, then `brew install --build-from-source msgs`
#      to check it before pushing)
#
# Installing from source instead of the bottle works today:
#   brew install --HEAD --build-from-source packaging/msgs.rb
class Msgs < Formula
  desc "Terminal client for iMessage on macOS"
  homepage "https://github.com/gautham-v/msgs"
  # No LICENSE file in the repository yet; add `license "..."` with one.
  version "0.1.0"
  head "https://github.com/gautham-v/msgs.git", branch: "main"

  # The tagged release: a universal binary, so one bottle covers Apple silicon
  # and Intel. Both fields are placeholders until the first tag is pushed.
  url "https://github.com/gautham-v/msgs/releases/download/v0.1.0/msgs-0.1.0-macos-universal.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  # macOS 14+, which is what msgs supports. SQLite is compiled in, so there is
  # nothing else to depend on.
  depends_on macos: :sonoma

  # Only needed to build from source or from HEAD; the release tarball is a
  # binary and needs neither.
  on_head do
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
