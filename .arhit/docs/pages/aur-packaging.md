AUR-пакет devforge (source build).

Файлы:
- packaging/aur/devforge/PKGBUILD — основной PKGBUILD: pkgname=devforge, source-build через cargo, depends=tmux+gcc-libs+glibc, makedepends=rust+cargo, статика embed-ится через rust-embed.
- packaging/aur/devforge/.SRCINFO — метаданные для AUR (генерируется через 'makepkg --printsrcinfo').
- packaging/aur/devforge/README.md — инструкция по локальной сборке (docker archlinux:latest) и первичной публикации в AUR (ssh://aur@aur.archlinux.org/devforge.git).

Релиз-флоу: после ./release.sh нужно вручную скопировать PKGBUILD+SRCINFO в клон AUR-репо, обновить pkgver, перегенерировать .SRCINFO, push. Автоматизация (PUBLISH_AUR=1 блок в release.sh) — TODO после регистрации AUR-аккаунта.

Установка пользователем: yay -S devforge / paru -S devforge. Документировано в README.md (раздел 'Установка через AUR').

Дополнения к стандартному PKGBUILD: options=('!lto') — workspace уже задаёт lto='thin' в profile.release, makepkg по умолчанию пытается передать -C lto, что конфликтует. --frozen в cargo build гарантирует строгую сборку по Cargo.lock.