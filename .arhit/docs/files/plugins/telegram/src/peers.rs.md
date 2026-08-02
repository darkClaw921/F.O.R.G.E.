# plugins/telegram/src/peers.rs

Резолв строкового dialog_id (Bot API-формат) в grammers PeerRef = идентичность + access_hash. Фаза 3.

Три эшелона от дешёвого к дорогому: state.peers (RwLock<HashMap<PeerId,PeerRef>>) -> FileSession::peer_ref (персист на диске) -> ограниченный iter_dialogs (DIALOG_SCAN_LIMIT=200, попутно наполняет кэш). Не нашли — 404 unknown_dialog.

resolve_peer_with(state, dialog_id, &dyn PeerSource) держит всю логику; trait PeerSource (lookup_session/scan_dialogs) подменяется в тестах считающим фейком, поэтому инвариант 'второй резолв не идёт ни в сессию, ни в сеть' проверяется счётчиком, а не глазами по логам. resolve_peer строит RuntimeSource из живого рантайма.

Контракт: форма id проверяется ПЕРВОЙ (мусор -> 404 в любом состоянии соединения; 503 здесь означал бы ложное 'переподключись и повтори'), затем кэш (отдаётся даже без рантайма), затем 503 not_connected.

ЛОВУШКА grammers: Dialog::peer_ref() делает auth().unwrap() и паникует на min-peer'ах — используется Peer::to_ref(), возвращающий Ok(None).
RwLock-guard'ы никогда не удерживаются через await (иначе медленный iter_dialogs блокирует все параллельные запросы).
