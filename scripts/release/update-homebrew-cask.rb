#!/usr/bin/env ruby
# frozen_string_literal: true

# Writes Casks/shelbi-desktop.rb in the Homebrew tap: the notarized desktop
# app's zip (Shelbi-<version>-arm64.zip, attached to the GitHub release by the
# release workflow's `desktop-release` job), its sha256, and a dependency on
# the tap's `shelbi` formula, which the app is a client of. The sibling of
# update-homebrew-formula.rb; the `homebrew-pr` job runs both into one tap PR.

require "optparse"
require "pathname"

options = {
  github_repo: "jlong/shelbi",
  tap: "jlong/shelbi",
  tap_dir: nil,
  version: ENV["RELEASE_VERSION"],
  sha256: nil,
  sha256_file: nil,
  url: nil
}

OptionParser.new do |parser|
  parser.banner = "Usage: update-homebrew-cask.rb --tap-dir DIR --version VERSION " \
                  "(--sha256 HEX | --sha256-file FILE) [--github-repo owner/repo] [--tap owner/name] [--url URL]"

  parser.on("--tap-dir DIR", "Path to the checked-out Homebrew tap repository") { |value| options[:tap_dir] = value }
  parser.on("--version VERSION", "Release version without the leading v") { |value| options[:version] = value }
  parser.on("--sha256 HEX", "SHA256 of Shelbi-<version>-arm64.zip") { |value| options[:sha256] = value }
  parser.on("--sha256-file FILE", "The zip's .sha256 file (\"<sha256>  <name>\")") { |value| options[:sha256_file] = value }
  parser.on("--github-repo REPO", "GitHub source repository, for example jlong/shelbi") { |value| options[:github_repo] = value }
  parser.on("--tap NAME", "The tap's name, for the formula dependency (owner/name)") { |value| options[:tap] = value }
  parser.on("--url URL", "Override the download URL (local testing only, e.g. file:///...)") { |value| options[:url] = value }
end.parse!

abort "missing --tap-dir" if options[:tap_dir].to_s.empty?
abort "missing --version" if options[:version].to_s.empty?
abort "version must be MAJOR.MINOR.PATCH, got #{options[:version]}" unless options[:version].match?(/\A\d+\.\d+\.\d+\z/)

version = options[:version]
zip_name = "Shelbi-#{version}-arm64.zip"

sha = options[:sha256]
if sha.to_s.empty? && options[:sha256_file]
  path = Pathname(options[:sha256_file])
  abort "sha256 file not found: #{path}" unless path.file?

  recorded, filename = path.read.strip.split(/\s+/, 2)
  filename = filename&.sub(/\A\*/, "")
  abort "#{path} is for #{filename}, expected #{zip_name}" if filename && filename != zip_name
  sha = recorded
end
abort "missing --sha256 or --sha256-file" if sha.to_s.empty?
abort "not a sha256: #{sha}" unless sha.match?(/\A\h{64}\z/)

# `#{version}` stays literal in the cask (Homebrew interpolates it), so a
# version bump touches only the `version` and `sha256` lines.
url = options[:url] || "https://github.com/#{options[:github_repo]}/releases/download/v\#{version}/Shelbi-\#{version}-arm64.zip"

cask = <<~RUBY
  cask "shelbi-desktop" do
    version "#{version}"
    sha256 "#{sha.downcase}"

    url "#{url}"
    name "Shelbi"
    desc "Native desktop client for the Shelbi agent orchestrator"
    homepage "https://github.com/#{options[:github_repo]}"

    livecheck do
      url :url
      strategy :github_latest
    end

    # `brew style` wants the depends_on stanzas in this (alphabetical) order.
    depends_on arch: :arm64
    # The app is a client of the CLI and its daemon, at the same version.
    depends_on formula: "#{options[:tap]}/shelbi"
    depends_on macos: :sequoia

    app "Shelbi.app"

    zap trash: [
      "~/.shelbi/desktop-update-check.json",
      "~/.shelbi/projects/*/desktop-window.json",
    ]
  end
RUBY

cask_path = Pathname(options[:tap_dir]) / "Casks" / "shelbi-desktop.rb"
cask_path.dirname.mkpath
cask_path.write(cask)

puts "updated #{cask_path}"
