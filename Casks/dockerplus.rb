cask "dockerplus" do
  version "0.1.0"
  sha256 "37c7321319c033ae33be27dc4ae508759f9af5f4ef7dac04a084cbbbcc920c9e"

  url "https://github.com/dockerplus/dockerplus/releases/download/v#{version}/DockerPlus-#{version}-arm64.zip"
  name "DockerPlus"
  desc "Small native app for Colima and its Docker engine"
  homepage "https://github.com/dockerplus/dockerplus"

  depends_on arch: :arm64
  depends_on formula: "colima"
  depends_on macos: :tahoe

  app "DockerPlus.app"
  # macOS kills a quarantined script on sight, and everything in the download is
  # quarantined. This wrapper is written here, and only reads that one.
  command_wrapper "dockerplus", content: <<~SH
    #!/bin/sh
    exec /bin/sh "#{appdir}/DockerPlus.app/Contents/Resources/dockerplus" "$@"
  SH

  zap trash: "~/Library/Preferences/local.dockerplus.app.plist"

  caveats <<~EOS
    DockerPlus is not notarized. If macOS blocks it, open System Settings,
    go to Privacy & Security and choose Open Anyway.

    It also needs a Docker command line. If you have none yet:
      brew install docker docker-compose
  EOS
end
