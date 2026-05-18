#!/bin/bash
#
# Release script for devforge (F.O.R.G.E.)
# Usage: ./release.sh <version>     e.g. ./release.sh 0.1.2
#
# Действия:
#   1) Бампает version в tmux-web/Cargo.toml.
#   2) Обновляет url в Formula/devforge.rb на новый тег.
#   3) Делает локальную сборку cargo build --release как smoke-test.
#   4) Коммитит изменения, ставит тег vX.Y.Z, пушит в origin/master.
#   5) Скачивает tarball релиза с GitHub и считает sha256.
#   6) Подставляет sha256 в локальный Formula/devforge.rb.
#   7) Клонирует tap-репо darkClaw921/homebrew-tap во временную директорию,
#      копирует туда обновлённую формулу, коммитит и пушит в main.
#
# Требования:
#   - git remote origin указывает на darkClaw921/F.O.R.G.E.
#   - права на push в darkClaw921/homebrew-tap (общий tap для всех формул)
#   - установлен rust toolchain, curl, shasum
#
set -euo pipefail

VERSION="${1:?Usage: ./release.sh <version>  (e.g. 0.1.2)}"
TAG="v${VERSION}"
REPO="darkClaw921/F.O.R.G.E."
TAP_REPO="darkClaw921/homebrew-tap"
FORMULA_NAME="devforge.rb"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${REPO_ROOT}"

echo "==> Updating version to ${VERSION} in project files..."
sed -i '' "s/^version = \"[^\"]*\"/version = \"${VERSION}\"/" tmux-web/Cargo.toml
sed -i '' "s|archive/refs/tags/v[^\"]*\.tar\.gz|archive/refs/tags/${TAG}.tar.gz|" "Formula/${FORMULA_NAME}"

echo "==> Building release binary (smoke test).."# Phase 1 (Echo): проект теперь Cargo workspace, собираем только бинарь
# devforge через -p. Команда запускается из корня workspace, чтобы cargo
# правильно резолвил workspace-root Cargo.toml и path-deps (forge-echo,
# echo-host-api в plugins/).
cargo build -p devforge --release

echo "==> Committing version bump (if any changes)..."
git add tmux-web/Cargo.toml tmux-web/Cargo.lock "Formula/${FORMULA_NAME}"
if ! git diff --cached --quiet; then
  git commit -m "Release ${TAG}"
else
  echo "    (no version-related changes to commit)"
fi

echo "==> Creating git tag ${TAG}..."
if git rev-parse "${TAG}" >/dev/null 2>&1; then
  echo "ERROR: tag ${TAG} already exists locally. Aborting."
  exit 1
fi
git tag "${TAG}"
git push origin master --tags

echo "==> Waiting for tag to propagate on GitHub..."
sleep 3

echo "==> Downloading tarball and computing sha256..."
TARBALL_URL="https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz"
SHA256=$(curl -fsSL "${TARBALL_URL}" | shasum -a 256 | awk '{print $1}')
echo "    tarball: ${TARBALL_URL}"
echo "    sha256:  ${SHA256}"

echo "==> Preparing formula copy with real sha256 for tap..."
FORMULA_OUT="$(mktemp -t devforge-formula-XXXXXX.rb)"
sed "s|sha256 \".*\"|sha256 \"${SHA256}\"|" "Formula/${FORMULA_NAME}" > "${FORMULA_OUT}"

echo "==> Publishing formula to tap ${TAP_REPO}..."
TAP_DIR=$(mktemp -d)
trap 'rm -rf "${TAP_DIR}"' EXIT

git clone "https://github.com/${TAP_REPO}.git" "${TAP_DIR}"

mkdir -p "${TAP_DIR}/Formula"
cp "${FORMULA_OUT}" "${TAP_DIR}/Formula/${FORMULA_NAME}"
rm -f "${FORMULA_OUT}"

cd "${TAP_DIR}"
git add -A
if ! git diff --cached --quiet; then
  git commit -m "Update devforge to ${TAG}"
  git push origin main
else
  echo "    (tap formula already up-to-date)"
fi
cd "${REPO_ROOT}"

echo ""
echo "==> Done. Users can install / update with:"
echo "      brew tap darkClaw921/tap        # один раз"
echo "      brew install devforge           # установка"
echo "      brew upgrade devforge           # обновление"
