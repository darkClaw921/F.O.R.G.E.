# AUR-пакет `devforge`

PKGBUILD для публикации `devforge` (F.O.R.G.E.) в [AUR](https://aur.archlinux.org).
Собирает бинарь из исходников через `cargo build -p devforge --release --frozen`.

## Локальная проверка

На Arch-системе:

```bash
cd packaging/aur/devforge
makepkg -si --noconfirm
```

Без Arch-машины (docker):

```bash
docker run --rm -v "$PWD:/pkg" -w /pkg archlinux:latest bash -c '
  pacman -Syu --noconfirm base-devel rust tmux git &&
  useradd -m b && chown -R b /pkg &&
  sudo -u b makepkg -s --noconfirm
'
```

После сборки появится файл `devforge-<ver>-1-x86_64.pkg.tar.zst`.
Установка в чистом контейнере:

```bash
docker run --rm -v "$PWD:/pkg" archlinux:latest bash -c \
  'pacman -U --noconfirm /pkg/devforge-*.pkg.tar.zst && devforge --help'
```

## Регенерация `.SRCINFO`

Каждый раз после правки PKGBUILD:

```bash
makepkg --printsrcinfo > .SRCINFO
```

## Первичная публикация в AUR

1. Зарегистрироваться на https://aur.archlinux.org, добавить SSH-публичный ключ.
2. Создать пустой AUR-репозиторий (имя должно совпадать с `pkgname=devforge`):

   ```bash
   git clone ssh://aur@aur.archlinux.org/devforge.git aur-devforge
   cd aur-devforge
   cp ../F.O.R.G.E./packaging/aur/devforge/PKGBUILD .
   cp ../F.O.R.G.E./packaging/aur/devforge/.SRCINFO .
   git add PKGBUILD .SRCINFO
   git commit -m "Initial import devforge 0.1.21"
   git push
   ```

3. Проверить страницу пакета: https://aur.archlinux.org/packages/devforge

## Обновление при релизе

После бампа версии в `tmux-web/Cargo.toml`:

1. В этом каталоге обновить `pkgver=` и (при необходимости) `sha256sums=` в `PKGBUILD`
   (`SKIP` оставляем — tarball на GitHub меняется при пересоздании тега;
   надёжнее проверять контрольную сумму, но это требует генерации после `gh release`).
2. `makepkg --printsrcinfo > .SRCINFO`.
3. Скопировать оба файла в локальный клон AUR-репо, закоммитить и запушить.

Автоматизация push в AUR из `release.sh` — TODO (включить env-флагом `PUBLISH_AUR=1`
после регистрации аккаунта).
