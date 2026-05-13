# Homebrew formula for devforge (F.O.R.G.E.)
#
# Builds the `devforge` binary from source via cargo. Static assets are
# embedded into the binary via rust-embed (see tmux-web/src/static_embed.rs),
# so no additional resource installation is required.
#
# -----------------------------------------------------------------------------
# ACTION REQUIRED (user, manual): fill in the real commit SHA of tag v0.1.0.
#
# The `revision:` field below holds a placeholder string. Replace it with the
# actual commit SHA produced when you create the v0.1.0 tag. Steps:
#
#     # In the root of the F.O.R.G.E. repo:
#     git tag v0.1.0
#     git push origin v0.1.0
#     git rev-parse v0.1.0
#     # Copy the 40-char SHA from the output and paste it below
#     # in place of REPLACE_WITH_v0.1.0_COMMIT_SHA.
#
# Full checklist: docs/homebrew-tap-setup.md (section "4. Релиз v0.1.0").
#
# Until the SHA is filled in, install locally via HEAD:
#     brew install --build-from-source --HEAD ./Formula/devforge.rb
# -----------------------------------------------------------------------------
class Devforge < Formula
  desc "Tmux + kanban + git web cockpit (F.O.R.G.E.)"
  homepage "https://github.com/darkClaw921/F.O.R.G.E."
  url "https://github.com/darkClaw921/F.O.R.G.E..git",
      tag:      "v0.1.0",
      revision: "REPLACE_WITH_v0.1.0_COMMIT_SHA"
  license "MIT"
  head "https://github.com/darkClaw921/F.O.R.G.E..git", branch: "master"

  depends_on "rust" => :build
  depends_on "tmux"

  def install
    cd "tmux-web" do
      system "cargo", "install", *std_cargo_args
    end
  end

  test do
    assert_match "devforge", shell_output("#{bin}/devforge --help 2>&1")
  end
end
