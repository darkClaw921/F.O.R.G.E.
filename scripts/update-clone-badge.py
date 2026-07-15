#!/usr/bin/env python3
"""Аккумулирует git clone-статистику (GitHub Traffic API) в сквозной
счётчик и рендерит его как shields.io endpoint-бейдж для README.

GitHub Traffic API (`/repos/<repo>/traffic/clones`) отдаёт только скользящее
окно 14 дней — сам по себе он не даёт "всего скачиваний с момента создания
репозитория". Поэтому скрипт держит `badges/_clones-history.json` —
накопленную по дням историю (ключ — дата `YYYY-MM-DD`, значение —
{count, uniques} за этот день), где каждый повторный запуск ПЕРЕЗАПИСЫВАЕТ
(а не суммирует) запись за конкретный день — иначе один день, попавший в
несколько запусков подряд (окно перекрывается), задвоился бы. Сумма по всем
дням в истории и есть отображаемое в бейдже число — растёт только за счёт
НОВЫХ дней, добавленных с момента первого запуска этого workflow.

Запускается:
  - по расписанию из .github/workflows/clone-stats.yml (GITHUB_TOKEN из
    secrets, есть Traffic API доступ на репозиторий, где идёт Action);
  - вручную для сидирования/отладки — тогда токен берётся из `gh auth
    token` (GH CLI должен быть залогинен с push-доступом к репозиторию).
"""
import json
import os
import subprocess
import urllib.request

REPO = os.environ.get("GITHUB_REPOSITORY", "darkClaw921/F.O.R.G.E.")
HISTORY_PATH = "badges/_clones-history.json"
BADGE_PATH = "badges/git-clones.json"


def resolve_token() -> str:
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        return token
    return subprocess.check_output(["gh", "auth", "token"], text=True).strip()


def fetch_clones(token: str) -> dict:
    req = urllib.request.Request(
        f"https://api.github.com/repos/{REPO}/traffic/clones",
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)


def load_history() -> dict:
    if os.path.exists(HISTORY_PATH):
        with open(HISTORY_PATH, encoding="utf-8") as f:
            return json.load(f)
    return {}


def write_json(path: str, data: dict) -> None:
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.write("\n")


def main() -> None:
    token = resolve_token()
    data = fetch_clones(token)

    history = load_history()
    for entry in data.get("clones", []):
        day = entry["timestamp"][:10]
        history[day] = {"count": entry["count"], "uniques": entry["uniques"]}

    os.makedirs("badges", exist_ok=True)
    write_json(HISTORY_PATH, history)

    total_clones = sum(v["count"] for v in history.values())
    write_json(
        BADGE_PATH,
        {
            "schemaVersion": 1,
            "label": "git clones",
            "message": str(total_clones),
            "color": "blue",
        },
    )
    print(f"total clones so far: {total_clones}")


if __name__ == "__main__":
    main()
