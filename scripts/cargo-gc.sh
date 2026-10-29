#!/usr/bin/env bash
# Уборка target/ — чтобы кэш сборки не рос бесконечно.
#
# Cargo НИКОГДА не удаляет артефакты сам: каждая смена версии зависимости,
# ветки или feature-набора добавляет в target/debug/deps новый .rlib/.rmeta,
# а старый остаётся навсегда. Отсюда десятки гигабайт за пару месяцев.
#
# Скрипт делает две вещи:
#   1. Всегда стирает target/*/incremental — кэш инкрементальной компиляции.
#      Он самый жирный и самый недолговечный: восстанавливается первой же
#      сборкой, терять там нечего.
#   2. Если после этого target/ всё ещё больше порога — делает полный
#      `cargo clean`. Точечно удалять старьё по mtime бессмысленно: cargo
#      не обновляет mtime у переиспользованных артефактов, так что «старым»
#      выглядит и то, что нужно прямо сейчас.
#
# Использование:
#   scripts/cargo-gc.sh          # порог по умолчанию — 20 ГиБ
#   scripts/cargo-gc.sh 40       # свой порог в ГиБ
#   scripts/cargo-gc.sh 0        # безусловный полный clean
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

LIMIT_GIB="${1:-20}"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"

if [[ ! -d "$TARGET" ]]; then
  echo "target/ отсутствует — чистить нечего"
  exit 0
fi

size_gib() { echo $(( $(du -sk "$TARGET" | cut -f1) / 1024 / 1024 )); }

before="$(size_gib)"
echo "target/: ${before} ГиБ (порог ${LIMIT_GIB} ГиБ)"

# 1. Кэш инкрементальной компиляции — всегда.
removed_incremental=0
for dir in "$TARGET"/*/incremental; do
  [[ -d "$dir" ]] || continue
  rm -rf "$dir"
  removed_incremental=1
done
[[ "$removed_incremental" == 1 ]] && echo "удалён incremental-кэш"

# 2. Полный clean, если всё ещё выше порога.
after="$(size_gib)"
if (( after >= LIMIT_GIB )); then
  echo "осталось ${after} ГиБ — полный cargo clean (следующая сборка будет с нуля)"
  cargo clean
else
  echo "осталось ${after} ГиБ — полный clean не нужен"
fi
