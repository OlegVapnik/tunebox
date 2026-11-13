# Made for each release from packaging/homebrew/tunebox.rb in https://github.com/OlegVapnik/tunebox.
# The Spotify build. tunebox-ytmusic has YouTube Music instead, tunebox-full both.
class Tunebox < Formula
  desc "Spotify client for the terminal"
  homepage "https://github.com/OlegVapnik/tunebox"
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-spotify-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256_SPOTIFY_AARCH64_APPLE_DARWIN@"
    end
    on_intel do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-spotify-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256_SPOTIFY_X86_64_APPLE_DARWIN@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-spotify-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256_SPOTIFY_AARCH64_UNKNOWN_LINUX_GNU@"
    end
    on_intel do
      url "https://github.com/OlegVapnik/tunebox/releases/download/v@VERSION@/tunebox-spotify-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256_SPOTIFY_X86_64_UNKNOWN_LINUX_GNU@"
    end
  end

  conflicts_with "tunebox-full", "tunebox-ytmusic", because: "each installs a tunebox binary"

  def install
    bin.install "tunebox"
  end

  def caveats
    text = <<~EOS
      tunebox needs Spotify Premium, and a free Spotify app of your own to read
      your library. Create the app and put its client ID into
      ~/.config/tunebox/config.toml, as https://github.com/OlegVapnik/tunebox#setup
      describes. Then sign in, and check that everything works:
        tunebox login
        tunebox doctor
    EOS
    if OS.linux?
      text += <<~EOS

        tunebox plays through PulseAudio or PipeWire, with your system's ALSA and
        PulseAudio libraries (libasound2 and libpulse0 on Debian and Ubuntu).
      EOS
    end
    text
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tunebox --version")
  end
end
