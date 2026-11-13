# Made for each release from packaging/homebrew/tunebox-ytmusic.rb in https://github.com/OlegVapnik/tunebox.
# The YouTube Music build, without Spotify. tunebox has Spotify instead, tunebox-full both.
class TuneboxYtmusic < Formula
  desc "YouTube Music client for the terminal"
  homepage "https://github.com/OlegVapnik/tunebox"
  version "@VERSION@"
  license "MIT"

  # mpv plays, yt-dlp finds the streams, and deno solves YouTube's challenges for yt-dlp.
  depends_on "deno"
  depends_on "mpv"
  depends_on "yt-dlp"

  on_macos do
    on_arm do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-ytmusic-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_YTMUSIC_AARCH64_APPLE_DARWIN@"
    end
    on_intel do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-ytmusic-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_YTMUSIC_X86_64_APPLE_DARWIN@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-ytmusic-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256_YTMUSIC_AARCH64_UNKNOWN_LINUX_GNU@"
    end
    on_intel do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-ytmusic-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256_YTMUSIC_X86_64_UNKNOWN_LINUX_GNU@"
    end
  end

  conflicts_with "tunebox", "tunebox-full", because: "each installs a tunebox binary"

  def install
    bin.install "tunebox"
  end

  def caveats
    <<~EOS
      tunebox plays YouTube Music without an account. To also see your library
      and likes, sign in: tunebox reads your browser's YouTube cookies, and they
      stay on this computer. Then check that everything works:
        tunebox login ytmusic
        tunebox doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tunebox --version")
  end
end
