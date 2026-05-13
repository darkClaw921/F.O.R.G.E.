# PtyHandle::writer_mut

Метод PtyHandle в tmux-web/src/pty.rs. Возвращает Option<&mut Box<dyn Write+Send>> — &mut на обёртку writer'а внутри Option.

pub fn writer_mut(&mut self) -> Option<&mut Box<dyn Write + Send>> {
    self.writer.as_mut()
}

## Зачем
ws.rs пишет WS-binary-input в PTY, не вынося writer наружу. Вся блокирующая запись делается под Mutex<Option<PtyHandle>> внутри tokio::task::spawn_blocking — это позволяет control-handler (resize/switch) и binary-handler разделять writer без race condition.

## Возвращаемый тип
Option<&mut Box<dyn Write+Send>>, а не Option<&mut (dyn Write+Send)> — потому что dyn-объекты в боксе по умолчанию имеют lifetime 'static, и as_deref_mut приводит к ошибке lifetime invariance. Доступ к Write idx через двойной deref: writer.write_all(&bytes) (Box auto-deref).
