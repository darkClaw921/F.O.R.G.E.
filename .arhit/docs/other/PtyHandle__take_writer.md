# PtyHandle::take_writer

Забирает blocking-writer PTY (transfers ownership of stdin writer). Возвращает Some(writer) при первом вызове, None — при повторных. Помечен #[allow(dead_code)]: сейчас не используется напрямую (writer берётся через writer_mut под Mutex<PtyHandle> в ws.rs), но сохранён как часть симметричного API с take_reader для будущих bridge'ей.
