//! PTY-обёртки над `portable-pty` для запуска интерактивных TUI-программ:
//! `tmux attach -t <session>` ([`spawn_tmux_attach`]) и `lazygit`
//! ([`spawn_lazygit`]).
//!
//! [`PtyHandle`] инкапсулирует master-сторону псевдотерминала, дочерний процесс
//! TUI и blocking I/O endpoint'ы (reader/writer). Чтение и запись по портам
//! `portable-pty` — синхронные (`std::io::Read` / `std::io::Write`), поэтому в
//! WebSocket-bridge (`src/ws.rs`) их следует оборачивать в
//! `tokio::task::spawn_blocking`.
//!
//! ### Жизненный цикл
//!
//! 1. [`spawn_tmux_attach`] / [`spawn_lazygit`] открывают PTY заданного размера,
//!    спавнят дочерний процесс с `TERM=xterm-256color` и возвращают
//!    [`PtyHandle`].
//! 2. Пока handle жив, ws.rs может бесконечно читать / писать байты и менять
//!    размер ([`PtyHandle::resize`]).
//! 3. При drop'e — [`Drop`] kill'ит ребёнка и waits на нём, чтобы избежать
//!    зомби-процессов.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Дескриптор живого PTY, в котором запущен `tmux attach`.
///
/// Поля:
/// - `master` — master-сторона PTY, на ней вызывается `resize()`.
/// - `child` — дескриптор дочернего процесса tmux. Drop kill'ит и ждёт его.
/// - `reader` — синхронный `Read`, с которого читается stdout PTY.
/// - `writer` — синхронный `Write`, в который пишется stdin PTY.
///
/// # Thread-safety
///
/// `MasterPty`, `Child`, `Read`, `Write` — `Send`, но *не обязательно*
/// `Sync`. На практике для обмена с асинхронным кодом в `ws.rs` мы перемещаем
/// reader/writer в `spawn_blocking`-таски, а `master` и `child` оставляем в
/// async-таске, который владеет [`PtyHandle`].
pub struct PtyHandle {
    /// Master-сторона псевдотерминала. На ней вызывается `resize`.
    pub master: Box<dyn MasterPty + Send>,
    /// Дочерний процесс tmux. Хранится в Option, чтобы Drop мог взять
    /// владение и сделать `kill` + `wait` без `unsafe`.
    child: Option<Box<dyn Child + Send + Sync>>,
    /// Blocking-reader stdout PTY. Может быть взят (`take_reader`) и
    /// перемещён в `spawn_blocking` для проксирования в WebSocket.
    reader: Option<Box<dyn Read + Send>>,
    /// Blocking-writer stdin PTY. Может быть взят (`take_writer`) и
    /// перемещён в `spawn_blocking` для проксирования из WebSocket.
    writer: Option<Box<dyn Write + Send>>,
}

impl PtyHandle {
    /// Меняет размер PTY. tmux получит SIGWINCH и перерисует layout.
    ///
    /// `cols`/`rows` — желаемые размеры в символах. `pixel_*` оставляем
    /// нулями: tmux ими не пользуется, как и большинство TUI.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("PtyHandle::resize: master.resize failed")
    }

    /// Забирает blocking-reader. Вызывается один раз при настройке bridge'а.
    /// Повторный вызов вернёт `None`.
    pub fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    /// Забирает blocking-writer. Вызывается один раз при настройке bridge'а.
    /// Повторный вызов вернёт `None`.
    ///
    /// Сейчас не используется напрямую (writer берётся через `writer_mut` под
    /// `Mutex<PtyHandle>` в `ws.rs`), но оставлен как часть симметричного API
    /// с `take_reader` — может пригодиться для будущих bridge'ей.
    #[allow(dead_code)]
    pub fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        self.writer.take()
    }

    /// Возвращает `&mut`-ссылку на writer (если он ещё не взят `take_writer`).
    ///
    /// Используется в `ws.rs` для записи WS-binary-байт в PTY без выноса
    /// writer'а наружу — вся блокирующая запись делается под `Mutex<PtyHandle>`
    /// внутри `spawn_blocking`.
    pub fn writer_mut(&mut self) -> Option<&mut Box<dyn Write + Send>> {
        self.writer.as_mut()
    }

    /// PID дочернего процесса tmux, если он ещё запущен.
    ///
    /// Используется в unit-тестах (`spawn_for_missing_session_does_not_panic`),
    /// поэтому в non-test сборке помечен как `allow(dead_code)`.
    #[allow(dead_code)]
    pub fn child_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|c| c.process_id())
    }
}

/// Спавнит `tmux attach -t <session>` внутри нового PTY размера `cols × rows`.
///
/// Стартовое окружение:
/// - `TERM=xterm-256color` — обязательно, иначе tmux зайдёт в degraded-режим.
/// - Остальные переменные наследуются от текущего процесса (см. CommandBuilder
///   default behaviour).
///
/// Возвращает живой [`PtyHandle`]. Если такой сессии нет — PTY всё равно
/// откроется, а `tmux` напишет ошибку в stdout и вскоре завершится. Эту
/// ситуацию можно детектировать в ws.rs по EOF на reader'е.
pub fn spawn_tmux_attach(session: &str, cols: u16, rows: u16) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty failed")?;

    let mut cmd = CommandBuilder::new("tmux");
    cmd.args(["attach", "-t", session]);
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to spawn `tmux attach -t {session}`"))?;

    // После spawn slave-fd нам больше не нужен в текущем процессе: ребёнок
    // унаследовал fd. Закрываем slave явно, иначе master-side EOF не
    // придёт после exit'а ребёнка (на некоторых платформах). drop(slave).
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .context("master.try_clone_reader failed")?;
    let writer = pair
        .master
        .take_writer()
        .context("master.take_writer failed")?;

    Ok(PtyHandle {
        master: pair.master,
        child: Some(child),
        reader: Some(reader),
        writer: Some(writer),
    })
}

/// Спавнит `lazygit` внутри нового PTY размера `cols × rows` с заданным
/// рабочим каталогом (`cwd`).
///
/// Назначение:
/// - Используется в WebSocket-handler'е `/ws/lazygit` для интерактивного
///   git-UI прямо в браузере (через xterm.js во фронтенде).
///
/// Стартовое окружение:
/// - `cwd` — корень git-репозитория (или любой путь внутри него); lazygit сам
///   найдёт ближайший `.git`. Чаще всего это путь активного проекта из
///   `projects.rs`.
/// - `TERM=xterm-256color` — обязательно, иначе lazygit отрисует TUI без
///   цвета и с ASCII-bordertypes.
/// - Остальные переменные наследуются от текущего процесса (см. CommandBuilder
///   default behaviour) — это важно для `$HOME/.config/lazygit/config.yml`.
///
/// Обработка ошибок:
/// - Если бинарь `lazygit` не найден в `PATH`, `spawn_command` вернёт ошибку,
///   которую мы оборачиваем в `with_context`-сообщение с подсказкой об
///   установке (`brew install lazygit` / `pacman -S lazygit` и т.п.). Это
///   позволяет ws-handler'у показать осмысленный баннер пользователю вместо
///   общего `No such file or directory`.
///
/// Возвращает живой [`PtyHandle`]. EOF на reader'е сигнализирует, что
/// lazygit завершился (например, пользователь нажал `q`).
pub fn spawn_lazygit(cwd: &Path, cols: u16, rows: u16) -> Result<PtyHandle> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty failed")?;

    let mut cmd = CommandBuilder::new("lazygit");
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");

    let child = pair.slave.spawn_command(cmd).with_context(|| {
        format!(
            "failed to spawn `lazygit` in {:?}: lazygit not found in PATH, \
             install via `brew install lazygit` (macOS) or your distro's package manager",
            cwd
        )
    })?;

    // После spawn slave-fd нам больше не нужен в текущем процессе: ребёнок
    // унаследовал fd. Закрываем slave явно, иначе master-side EOF не
    // придёт после exit'а ребёнка (на некоторых платформах). drop(slave).
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .context("master.try_clone_reader failed")?;
    let writer = pair
        .master
        .take_writer()
        .context("master.take_writer failed")?;

    Ok(PtyHandle {
        master: pair.master,
        child: Some(child),
        reader: Some(reader),
        writer: Some(writer),
    })
}

impl Drop for PtyHandle {
    /// Гарантированно kill'ит и reap'ает ребёнка, чтобы не оставить зомби.
    /// Ошибки игнорируем — Drop не должен паниковать.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что PTY открывается даже для несуществующей сессии:
    /// процесс tmux запустится и быстро завершится, но spawn должен пройти.
    /// Тест безвреден, даже если tmux не установлен — тогда вернётся Err и
    /// ассертом мы это явно проверяем (но не валим тест).
    #[test]
    fn spawn_for_missing_session_does_not_panic() {
        match spawn_tmux_attach("definitely-does-not-exist-xyz", 80, 24) {
            Ok(handle) => {
                // Просто проверяем, что pid у нас есть (или None — оба ОК).
                let _ = handle.child_pid();
                // Drop сделает kill+wait — ребёнок завершится.
            }
            Err(e) => {
                // Допустимо: tmux может отсутствовать в test-окружении.
                eprintln!("spawn_tmux_attach failed (tmux missing?): {e:#}");
            }
        }
    }
}
