# ws::spawn_pty_reader

Запускает spawn_blocking-задачу для синхронного чтения PTY и отправки чанков в mpsc-канал. Расположена в tmux-web/src/ws.rs. Финальная версия после bug-fix forge-qs0.

## Сигнатура

async fn spawn_pty_reader(
  pty: &Arc<Mutex<Option<PtyHandle>>>,
  tx: mpsc::Sender<Vec<u8>>,
  cancel: Arc<AtomicBool>,
  eof_notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()>

## Алгоритм

1. Берёт reader из PtyHandle::take_reader() (one-shot, под Mutex). Если pty уже None или reader уже забран — eof_notify.notify_one() и возвращает завершённый handle.
2. tokio::task::spawn_blocking { ... } — внутри:
   - buf = vec![0u8; 8 KiB]
   - natural_death = false
   - loop {
       if cancel { break; }
       match reader.read(&mut buf) {
         Ok(0) => { tracing::debug!('pty reader EOF'); natural_death=true; break; }
         Ok(n) => { tx.blocking_send(buf[..n].to_vec()).map_err(...) }
         Err(Interrupted) => continue;
         Err(_) => { tracing::debug!('pty reader error'); natural_death=true; break; }
       }
     }
   - if natural_death && !cancel { eof_notify.notify_one(); }

## Ключевая логика

- cancel-флаг проверяется между read'ами и при error/EOF — отделяет legitimate cleanup (switch/teardown ставят cancel=true и НЕ хотят будить главный loop) от unexpected death (внешний kill-session: cancel=false, нужно разбудить main loop через notify).
- blocking_send из spawn_blocking — потому что reader sync, а tx — async mpsc::Sender; blocking_send работает из sync контекста.
- При закрытом канале (writer-task ушёл) — break без notify (это уже teardown).
- Interrupted errors (signal) игнорируются — продолжаем читать.

## Контракт с handle_socket

- При внешней смерти tmux (kill-session) — notify_one() будит главный select! → handler закрывает WS.
- При cancel=true (switch/teardown) — НЕ notify (handler сам управляет lifecycle).
- При normal exit главный handler делает cancel.store(true) → reader выходит на следующей итерации (либо если уже в read() — Drop PtyHandle закрывает FD, read возвращает 0/Err).
