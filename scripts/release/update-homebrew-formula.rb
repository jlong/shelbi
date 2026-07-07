#!/usr/bin/env ruby
# frozen_string_literal: true

require "optparse"
require "pathname"

options = {
  github_repo: "jlong/shelbi",
  tap_dir: nil,
  version: ENV["RELEASE_VERSION"],
  checksums: "checksums.txt"
}

OptionParser.new do |parser|
  parser.banner = "Usage: update-homebrew-formula.rb --tap-dir DIR --version VERSION --checksums checksums.txt [--github-repo owner/repo]"

  parser.on("--tap-dir DIR", "Path to the checked-out Homebrew tap repository") { |value| options[:tap_dir] = value }
  parser.on("--version VERSION", "Release version without the leading v") { |value| options[:version] = value }
  parser.on("--checksums FILE", "sha256sum-format checksums file") { |value| options[:checksums] = value }
  parser.on("--github-repo REPO", "GitHub source repository, for example jlong/shelbi") { |value| options[:github_repo] = value }
end.parse!

abort "missing --tap-dir" if options[:tap_dir].to_s.empty?
abort "missing --version" if options[:version].to_s.empty?

checksums_path = Pathname(options[:checksums])
abort "checksums file not found: #{checksums_path}" unless checksums_path.file?

checksums = {}
checksums_path.each_line do |line|
  next if line.strip.empty?

  sha, filename = line.strip.split(/\s+/, 2)
  filename = filename&.sub(/\A\*/, "")
  next if sha.nil? || filename.nil?

  checksums[filename] = sha
end

version = options[:version]
github_repo = options[:github_repo]
release_base_url = "https://github.com/#{github_repo}/releases/download/v#{version}"
arm64_archive = "shelbi_Darwin_arm64.tar.gz"
x86_64_archive = "shelbi_Darwin_x86_64.tar.gz"

arm64_sha = checksums[arm64_archive]
x86_64_sha = checksums[x86_64_archive]

abort "missing #{arm64_archive} in #{checksums_path}" if arm64_sha.to_s.empty?

urls = if x86_64_sha
  <<~RUBY.chomp
    if Hardware::CPU.arm?
      url "#{release_base_url}/#{arm64_archive}"
      sha256 "#{arm64_sha}"
    else
      url "#{release_base_url}/#{x86_64_archive}"
      sha256 "#{x86_64_sha}"
    end
  RUBY
else
  <<~RUBY.chomp
    if Hardware::CPU.arm?
      url "#{release_base_url}/#{arm64_archive}"
      sha256 "#{arm64_sha}"
    else
      odie "Shelbi's Homebrew formula currently supports macOS arm64 only."
    end
  RUBY
end

formula = <<~RUBY
  class Shelbi < Formula
    desc "Open-source agent orchestrator built on tmux"
    homepage "https://github.com/#{github_repo}"
    version "#{version}"
    license "MIT"

    depends_on "tmux"

    on_macos do
  #{urls.gsub(/^/, "    ")}
    end

    def install
      bin.install "shelbi"
    end

    test do
      assert_match version.to_s, shell_output("\#{bin}/shelbi --version")
    end
  end
RUBY

formula_path = Pathname(options[:tap_dir]) / "Formula" / "shelbi.rb"
formula_path.dirname.mkpath
formula_path.write(formula)

puts "updated #{formula_path}"
