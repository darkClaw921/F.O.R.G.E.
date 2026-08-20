#!/bin/bash
#
# Release script for devforge (F.O.R.G.E.)
# Usage:
#   ./release.sh              # auto-bump patch версии (CURRENT → CURRENT+1)
#   ./release.sh 0.2.0        # явно задать target-версию
#
# Действия:
#   1) Определяет TARGET версию (auto-bump patch или из аргумента).
#   2) Pre-commit hook (.githooks/pre-commit) сам инкрементирует patch при
#      коммите, поэтому скрипт:
#        - в auto-режиме НЕ трогает Cargo.toml до коммита (хук бампнёт сам);
#        - в режиме с явной версией X.Y.Z выставляет Cargo.toml=(X.Y.Z-1)
#          ТОЛЬКО если хук должен довести до X.Y.Z; для нестандартного
#          скачка (например 0.1.19 → 0.2.0) скрипт записывает TARGET
#          напрямую И обходит логику автобампа выставлением Cargo.toml в
#          такое значение, что хук довёл бы его до TARGET. Если автобамп
#          до TARGET невозможен (TARGET ≠ CURRENT+1 patch) — скрипт идёт
#          в "manual"-режим: записывает (TARGET-1).patch в Cargo.toml,
#          где (TARGET-1).patch получается обратным шагом по patch.
#   3) Обновляет Formula/devforge.rb (URL на новый тег).
#   4) cargo build -p devforge --release как smoke-test.
#   5) Стейджит изменения, commit "Release vTARGET" → pre-commit бампает
#      Cargo.toml/Cargo.lock и добавляет в коммит.
#   6) Сверяет post-commit версию с TARGET. Если не совпало — abort.
#   7) Тэг vTARGET, push origin master --tags.
#   8) Скачивает tarball релиза, считает sha256.
#   9) Создаёт GitHub Release vTARGET (gh release create или REST API
#      через curl с $GITHUB_TOKEN/$GH_TOKEN).
#  10) Обновляет Formula в darkClaw921/homebrew-tap с реальным sha256.
#
# Требования:
#   - git remote origin = darkClaw921/F.O.R.G.E.
#   - push-доступ к darkClaw921/homebrew-tap
#   - rust toolchain, curl, shasum, awk
#   - для GitHub Release: либо `gh` (brew install gh), либо
#     env GITHUB_TOKEN/GH_TOKEN с правом repo:public_repo.

set -euo pipefail

REPO="darkClaw921/F.O.R.G.E."
TAP_REPO="darkClaw921/homebrew-tap"
FORMULA_NAME="devforge.rb"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${REPO_ROOT}"

CARGO_TOML="tmux-web/Cargo.toml"
CARGO_LOCK="Cargo.lock"

# ----------------------------------------------------------------------
# AUR: проверка доступности и публикация
# ----------------------------------------------------------------------
#
# Почему проверка идёт через `git ls-remote`, а не через `ssh ... help`:
# приветственная команда отвечает и когда сервис лежит на техобслуживании
# («The AUR is down due to maintenance»), и её код возврата не описывает
# то, что нам нужно на самом деле — сможем ли мы клонировать и запушить
# репозиторий пакета. Проверяем ровно ту операцию, которую собираемся
# делать.
#
# Печатает диагноз, различая три разные причины отказа: они требуют
# разных действий от человека, и общее «нет доступа» отправляет чинить
# то, что не сломано.
aur_reachable() {
    local out rc attempt=1 max=6 delay=15
    # ПОВТОРЫ, а не одна попытка. AUR троттлит частые подключения и рвёт их
    # на этапе обмена версиями: `kex_exchange_identification: Connection reset
    # by peer`. Это НЕ отказ доступа и НЕ техобслуживание — соседняя попытка
    # проходит. На релизе v0.1.69 одна попытка сорвалась, публикация в AUR
    # была пропущена, хотя сервис работал.
    #
    # Бюджет 15+30+60+120+240 = ~7,5 минуты, а не 75 секунд: после серии
    # подключений AUR блокирует источник НАДОЛГО (проверено — рвётся даже
    # соединение без BatchMode, при живом TCP на порту 22). Короткий бюджет
    # лишь тратит попытки и продлевает блокировку.
    while :; do
        out=$(GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=15" \
            git ls-remote "ssh://aur@aur.archlinux.org/devforge.git" 2>&1) && rc=0 || rc=$?
        if [ "${rc}" = "0" ]; then
            return 0
        fi
        if printf '%s' "${out}" \
            | grep -qi "connection reset\|kex_exchange_identification\|closed by remote host\|timed out\|broken pipe" \
            && [ "${attempt}" -lt "${max}" ]; then
            echo "    AUR сбросил соединение (попытка ${attempt}/${max}), повтор через ${delay}s..." >&2
            sleep "${delay}"
            delay=$(( delay * 2 ))
            attempt=$(( attempt + 1 ))
            continue
        fi
        break
    done
    if printf '%s' "${out}" | grep -qi "maintenance\|is down"; then
        echo "    причина: сам aur.archlinux.org недоступен (техобслуживание)." >&2
        echo "            Ключ и ~/.ssh/config здесь ни при чём — чинить нечего," >&2
        echo "            дождитесь подъёма сервиса и догоните: AUR_ONLY=1 ./release.sh" >&2
    elif printf '%s' "${out}" | grep -qi "permission denied\|publickey"; then
        echo "    причина: ключ не принят сервером." >&2
        echo "            Проверьте ~/.ssh/config (Host aur.archlinux.org →" >&2
        echo "            IdentityFile ~/.ssh/aur_devforge) и что публичная часть" >&2
        echo "            добавлена в профиль на aur.archlinux.org." >&2
    elif printf '%s' "${out}" | grep -qi "connection reset\|kex_exchange_identification\|closed by remote host"; then
        echo "    причина: AUR рвал соединение все ${max} попыток (троттлинг)." >&2
        echo "            Сервис жив, ключ ни при чём. Догнать позже:" >&2
        echo "            AUR_ONLY=1 ./release.sh <version>" >&2
    else
        echo "    причина: не удалось связаться с AUR." >&2
        printf '            %s\n' "${out}" | head -3 >&2
    fi
    return 1
}

# publish_aur <версия> <sha256> — доводит PKGBUILD/.SRCINFO в AUR до версии.
# Ничего не коммитит в основном репозитории, кроме синхронизации .SRCINFO.
publish_aur() {
    local target="$1" sha="$2"
    local aur_local_pkgbuild="${REPO_ROOT}/packaging/aur/devforge/PKGBUILD"

    if [ ! -f "${aur_local_pkgbuild}" ]; then
        echo "WARN: ${aur_local_pkgbuild} не найден — пропускаю AUR-публикацию." >&2
        return 1
    fi
    if ! command -v docker >/dev/null 2>&1; then
        echo "WARN: docker не найден — не могу сгенерировать .SRCINFO; пропускаю AUR." >&2
        echo "      Установите docker или выставьте SKIP_AUR=1." >&2
        return 1
    fi
    if ! aur_reachable; then
        echo "WARN: AUR пропущен." >&2
        return 1
    fi

    local aur_dir
    aur_dir="$(mktemp -d)"

    git clone ssh://aur@aur.archlinux.org/devforge.git "${aur_dir}" || {
        rm -rf "${aur_dir}"; return 1
    }

    cp "${aur_local_pkgbuild}" "${aur_dir}/PKGBUILD"
    sed -i.bak \
        -e "s|^pkgver=.*|pkgver=${target}|" \
        -e "s|^pkgrel=.*|pkgrel=1|" \
        -e "s|^sha256sums=.*|sha256sums=('${sha}')|" \
        "${aur_dir}/PKGBUILD"
    rm -f "${aur_dir}/PKGBUILD.bak"

    # .SRCINFO генерируется makepkg'ом, которого на macOS нет, — поэтому
    # archlinux-контейнер. --security-opt seccomp=unconfined нужен Docker
    # Desktop на macOS (иначе pacman падает на seccomp).
    echo "    Generating .SRCINFO via docker archlinux:latest..."
    docker run --rm --platform linux/amd64 --security-opt seccomp=unconfined \
        -v "${aur_dir}:/pkg" -w /pkg archlinux:latest bash -c '
            sed -i "s/^DownloadUser/#DownloadUser/" /etc/pacman.conf || true
            pacman -Syu --noconfirm --needed --quiet pacman-contrib sudo >/dev/null 2>&1
            useradd -m srcuser 2>/dev/null || true
            chown -R srcuser:srcuser /pkg
            sudo -u srcuser bash -c "cd /pkg && makepkg --printsrcinfo > .SRCINFO"
            chown -R 0:0 /pkg
        ' || { rm -rf "${aur_dir}"; return 1; }

    # Синхронизируем .SRCINFO в основной репозиторий — чтобы содержимое AUR
    # и packaging/aur/ не разъезжались между релизами.
    cp "${aur_dir}/.SRCINFO" "${REPO_ROOT}/packaging/aur/devforge/.SRCINFO"

    if [ ! -f "${aur_dir}/.gitignore" ]; then
        cat > "${aur_dir}/.gitignore" <<'AUR_GITIGNORE'
*.pkg.tar.zst
*.tar.gz
src/
pkg/
AUR_GITIGNORE
    fi

    (
        cd "${aur_dir}"
        git add PKGBUILD .SRCINFO .gitignore
        if ! git diff --cached --quiet; then
            git -c user.email="darkclaw921@users.noreply.github.com" \
                -c user.name="darkClaw921" \
                commit -m "Release v${target}"
            git push origin master
            echo "    ✓ AUR обновлён: https://aur.archlinux.org/packages/devforge"
        else
            echo "    (AUR PKGBUILD already up-to-date)"
        fi
    ) || { rm -rf "${aur_dir}"; return 1; }

    rm -rf "${aur_dir}"
    return 0
}

# ----------------------------------------------------------------------
# 1) Определение TARGET-версии
# ----------------------------------------------------------------------

current=$(awk -F'"' '/^version = / { print $2; exit }' "${CARGO_TOML}")
if [ -z "${current}" ]; then
    echo "ERROR: cannot parse current version from ${CARGO_TOML}" >&2
    exit 1
fi
if ! [[ "${current}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "ERROR: current version '${current}' is not X.Y.Z" >&2
    exit 1
fi
CUR_MAJOR="${BASH_REMATCH[1]}"
CUR_MINOR="${BASH_REMATCH[2]}"
CUR_PATCH="${BASH_REMATCH[3]}"

# ----------------------------------------------------------------------
# 1a) AUR_ONLY: догнать AUR для УЖЕ выпущенной версии
# ----------------------------------------------------------------------
# Нужен, когда релиз прошёл, а шаг AUR не отработал — например,
# aur.archlinux.org лежал на техобслуживании. Без этого режима догнать
# AUR можно было бы только выпустив новый релиз, что абсурдно: тег,
# GitHub Release и формула уже на месте и трогать их незачем.
#
#   AUR_ONLY=1 ./release.sh          # версия берётся из Cargo.toml
#   AUR_ONLY=1 ./release.sh 0.1.55   # явно
if [ "${AUR_ONLY:-0}" = "1" ]; then
    TARGET="${1:-${current}}"
    TAG="v${TARGET}"
    echo "==> AUR_ONLY: догоняю AUR до ${TAG} (релиз заново НЕ выпускается)"
    if ! git ls-remote --tags origin "${TAG}" 2>/dev/null | grep -q "${TAG}"; then
        echo "ERROR: тега ${TAG} нет на origin — сначала выпустите релиз." >&2
        exit 1
    fi
    SHA256=$(curl -fsSL "https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz" \
        | shasum -a 256 | awk '{print $1}')
    echo "    sha256: ${SHA256}"
    if publish_aur "${TARGET}" "${SHA256}"; then
        echo "==> Done. AUR на ${TAG}."
        exit 0
    fi
    exit 1
fi

if [ $# -ge 1 ] && [ -n "$1" ]; then
    TARGET="$1"
    if ! [[ "${TARGET}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
        echo "ERROR: target version '${TARGET}' is not X.Y.Z" >&2
        exit 1
    fi
    T_MAJOR="${BASH_REMATCH[1]}"
    T_MINOR="${BASH_REMATCH[2]}"
    T_PATCH="${BASH_REMATCH[3]}"
    AUTO_BUMP=0
else
    T_MAJOR="${CUR_MAJOR}"
    T_MINOR="${CUR_MINOR}"
    T_PATCH=$((CUR_PATCH + 1))
    TARGET="${T_MAJOR}.${T_MINOR}.${T_PATCH}"
    AUTO_BUMP=1
fi

TAG="v${TARGET}"

echo "==> Current version: ${current}"
echo "==> Target  version: ${TARGET}"
if [ "${AUTO_BUMP}" = "1" ]; then
    echo "    (auto-bump: pre-commit hook доведёт ${current} → ${TARGET})"
fi

# ----------------------------------------------------------------------
# 2) Pre-flight: тег не должен уже существовать
# ----------------------------------------------------------------------
if git rev-parse "${TAG}" >/dev/null 2>&1; then
    echo "ERROR: tag ${TAG} already exists locally. Aborting." >&2
    exit 1
fi
if git ls-remote --tags origin "${TAG}" 2>/dev/null | grep -q "${TAG}"; then
    echo "ERROR: tag ${TAG} already exists on origin. Aborting." >&2
    exit 1
fi

# ----------------------------------------------------------------------
# 3) Подгон Cargo.toml для нестандартных скачков (когда auto-bump
#    pre-commit hook'а не приведёт нас в TARGET)
# ----------------------------------------------------------------------
# Логика hook: читает текущую версию X.Y.Z из Cargo.toml и пишет X.Y.(Z+1).
# Чтобы после коммита Cargo.toml оказался = TARGET, надо чтобы в момент
# старта commit'а Cargo.toml содержал PRE = M.N.(P-1) где TARGET=M.N.P
# (если P≥1 — простой случай). Для major/minor bump'ов P=0 → нельзя
# вычесть из patch; тогда выставляем CARGO.toml = M.N.0 заранее, а
# хук бампнёт до M.N.1 — это уже НЕ TARGET, и скрипт упадёт на проверке.
#
# Поэтому при manual target допускаем только варианты, совместимые с
# автобампом: TARGET.patch ≥ 1. Иначе ругаемся.
if [ "${T_PATCH}" -lt 1 ]; then
    echo "ERROR: target ${TARGET} имеет patch=0, что несовместимо с auto-bump pre-commit hook'a." >&2
    echo "       Сначала временно отключите hook, либо целевая версия должна иметь patch ≥ 1." >&2
    exit 1
fi

PRE_PATCH=$((T_PATCH - 1))
PRE_VERSION="${T_MAJOR}.${T_MINOR}.${PRE_PATCH}"

if [ "${current}" != "${PRE_VERSION}" ]; then
    echo "==> Подгоняю ${CARGO_TOML} ${current} → ${PRE_VERSION} (hook довёдёт до ${TARGET})..."
    awk -v old="${current}" -v new="${PRE_VERSION}" '
        !done && $0 == "version = \"" old "\"" {
            sub("\"" old "\"", "\"" new "\"")
            done = 1
        }
        { print }
    ' "${CARGO_TOML}" > "${CARGO_TOML}.tmp" && mv "${CARGO_TOML}.tmp" "${CARGO_TOML}"
fi

# ----------------------------------------------------------------------
# 4) Обновление Formula URL
# ----------------------------------------------------------------------
echo "==> Updating Formula/${FORMULA_NAME} URL → ${TAG}..."
sed -i '' "s|archive/refs/tags/v[^\"]*\.tar\.gz|archive/refs/tags/${TAG}.tar.gz|" "Formula/${FORMULA_NAME}"

# ----------------------------------------------------------------------
# 5) Smoke-сборка
# ----------------------------------------------------------------------
echo "==> Building release binary (smoke test)..."
# Phase 1 Echo: проект — Cargo workspace, собираем только бинарь devforge
# через -p из корня workspace.
cargo build -p devforge --release

# ----------------------------------------------------------------------
# 6) Stage + commit
# ----------------------------------------------------------------------
echo "==> Staging release files..."
git add "${CARGO_TOML}" "${CARGO_LOCK}" "Formula/${FORMULA_NAME}"

if git diff --cached --quiet; then
    echo "ERROR: нечего коммитить. Возможно, версия уже совпадает с TARGET (${TARGET})." >&2
    echo "       Если хотите ре-релиз — внесите изменение либо используйте git tag вручную." >&2
    exit 1
fi

echo "==> Committing 'Release ${TAG}' (pre-commit hook поднимет version)..."
git commit -m "Release ${TAG}"

# ----------------------------------------------------------------------
# 7) Верификация post-commit версии
# ----------------------------------------------------------------------
post_version=$(awk -F'"' '/^version = / { print $2; exit }' "${CARGO_TOML}")
if [ "${post_version}" != "${TARGET}" ]; then
    echo "ERROR: после коммита Cargo.toml=${post_version}, ожидался ${TARGET}." >&2
    echo "       Скорее всего pre-commit hook повёл себя нестандартно. Релиз aborted." >&2
    echo "       Откатите коммит: git reset --soft HEAD~1" >&2
    exit 1
fi
echo "    ✓ Cargo.toml == ${TARGET}"

# ----------------------------------------------------------------------
# 8) Tag + push
# ----------------------------------------------------------------------
echo "==> Creating git tag ${TAG}..."
git tag "${TAG}"
git push origin master --tags

echo "==> Waiting for tag to propagate on GitHub..."
sleep 3

# ----------------------------------------------------------------------
# 9) Скачивание tarball и подсчёт sha256
# ----------------------------------------------------------------------
echo "==> Downloading tarball and computing sha256..."
TARBALL_URL="https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz"
SHA256=$(curl -fsSL "${TARBALL_URL}" | shasum -a 256 | awk '{print $1}')
echo "    tarball: ${TARBALL_URL}"
echo "    sha256:  ${SHA256}"

# ----------------------------------------------------------------------
# 10) GitHub Release
# ----------------------------------------------------------------------
echo "==> Creating GitHub Release ${TAG}..."

# Сбор release notes из коммитов между предыдущим тегом и текущим.
PREV_TAG=$(git tag --sort=-version:refname | grep -v "^${TAG}\$" | head -n 1 || true)
if [ -n "${PREV_TAG}" ]; then
    RELEASE_NOTES=$(git log --pretty=format:"- %s (%h)" "${PREV_TAG}..${TAG}")
    NOTES_HEADER="## Changes since ${PREV_TAG}"
else
    RELEASE_NOTES=$(git log --pretty=format:"- %s (%h)" "${TAG}")
    NOTES_HEADER="## Initial release"
fi

RELEASE_BODY=$(printf "%s\n\n%s\n\n---\n\n## Install via Homebrew\n\n\`\`\`\nbrew tap darkClaw921/tap\nbrew install devforge\n\`\`\`\n\nUpgrade:\n\`\`\`\nbrew upgrade devforge\n\`\`\`\n" "${NOTES_HEADER}" "${RELEASE_NOTES}")

if command -v gh >/dev/null 2>&1; then
    echo "    using: gh release create"
    printf "%s" "${RELEASE_BODY}" | gh release create "${TAG}" \
        --repo "${REPO}" \
        --title "${TAG}" \
        --notes-file - \
        --verify-tag
else
    GH_TOKEN_VAL="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
    if [ -z "${GH_TOKEN_VAL}" ]; then
        echo "WARN: ни gh CLI, ни GITHUB_TOKEN/GH_TOKEN не найдены — пропускаю GitHub Release." >&2
        echo "      Тэг уже запушен (${TAG}); release можно создать вручную:" >&2
        echo "        https://github.com/${REPO}/releases/new?tag=${TAG}" >&2
    else
        echo "    using: REST API (curl)"
        # JSON-encode тело через jq
        PAYLOAD=$(jq -n \
            --arg tag "${TAG}" \
            --arg name "${TAG}" \
            --arg body "${RELEASE_BODY}" \
            '{tag_name: $tag, name: $name, body: $body, draft: false, prerelease: false}')

        HTTP_CODE=$(curl -sS -o /tmp/release_resp.json -w "%{http_code}" \
            -X POST \
            -H "Accept: application/vnd.github+json" \
            -H "Authorization: Bearer ${GH_TOKEN_VAL}" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            "https://api.github.com/repos/${REPO}/releases" \
            -d "${PAYLOAD}")

        if [ "${HTTP_CODE}" = "201" ]; then
            RELEASE_URL=$(jq -r '.html_url' /tmp/release_resp.json)
            echo "    ✓ Release: ${RELEASE_URL}"
        else
            echo "WARN: GitHub Release API вернул HTTP ${HTTP_CODE}:" >&2
            cat /tmp/release_resp.json >&2 || true
            echo "      Тэг ${TAG} уже запушен — release можно создать вручную." >&2
        fi
        rm -f /tmp/release_resp.json
    fi
fi

# ----------------------------------------------------------------------
# 11) Tap formula update
# ----------------------------------------------------------------------
echo "==> Preparing formula copy with real sha256 for tap..."
FORMULA_OUT="$(mktemp -t devforge-formula-XXXXXX.rb)"
sed "s|sha256 \".*\"|sha256 \"${SHA256}\"|" "Formula/${FORMULA_NAME}" > "${FORMULA_OUT}"

echo "==> Publishing formula to tap ${TAP_REPO}..."
TAP_DIR=$(mktemp -d)
trap 'rm -rf "${TAP_DIR}" "${FORMULA_OUT}"' EXIT

git clone "https://github.com/${TAP_REPO}.git" "${TAP_DIR}"

mkdir -p "${TAP_DIR}/Formula"
cp "${FORMULA_OUT}" "${TAP_DIR}/Formula/${FORMULA_NAME}"

cd "${TAP_DIR}"
git add -A
if ! git diff --cached --quiet; then
    git commit -m "Update devforge to ${TAG}"
    git push origin main
else
    echo "    (tap formula already up-to-date)"
fi
cd "${REPO_ROOT}"

# ----------------------------------------------------------------------
# 12) AUR (Arch User Repository) update — packaging/aur/devforge → AUR git
# ----------------------------------------------------------------------
#
# Толкает PKGBUILD/.SRCINFO в ssh://aur@aur.archlinux.org/devforge.git.
# Требования:
#   - SSH-доступ к AUR (ключ ~/.ssh/aur_devforge или Host aur.archlinux.org
#     в ~/.ssh/config; см. packaging/aur/devforge/README.md).
#   - docker — для генерации .SRCINFO через makepkg --printsrcinfo
#     (на macOS makepkg отсутствует).
#
# Отключить шаг: SKIP_AUR=1 ./release.sh

AUR_OK=1
if [ "${SKIP_AUR:-0}" = "1" ]; then
    echo "==> AUR push skipped (SKIP_AUR=1)."
else
    echo "==> Updating AUR package devforge → ${TAG}..."
    # Сбой AUR НЕ роняет релиз: тег, GitHub Release и формула к этому моменту
    # уже опубликованы, и ненулевой код возврата всего скрипта означал бы
    # «релиз не удался», хотя он удался. Отставший AUR догоняется отдельно.
    publish_aur "${TARGET}" "${SHA256}" || AUR_OK=0
fi

echo ""
echo "==> Done. Released ${TAG}."
echo "    GitHub:  https://github.com/${REPO}/releases/tag/${TAG}"
echo "    Install: brew tap darkClaw921/tap && brew install devforge"
echo "    Upgrade: brew upgrade devforge"
if [ "${AUR_OK}" = "1" ]; then
    echo "    AUR:     yay -S devforge   (https://aur.archlinux.org/packages/devforge)"
else
    echo ""
    echo "    ВНИМАНИЕ: AUR остался на прежней версии — релиз это НЕ отменяет."
    echo "    Догнать, когда причина выше устранена:  AUR_ONLY=1 ./release.sh ${TARGET}"
fi
