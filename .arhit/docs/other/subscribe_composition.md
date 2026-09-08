# subscribe_composition

Сигнал изменения СОСТАВА реестра порталов Bitrix24 (plugins/bitrix/src/registry.rs, Ф5).

B24Registry держит tokio::sync::watch::Sender<u64> с монотонным поколением. bump_composition вызывается из add (включая занятие пустого слота и обновление title), remove, rename и reorder; send_modify, а не send — «никто не слушает» это норма, а не сбой добавления портала.

API: subscribe_composition() -> watch::Receiver<u64>, composition_generation() -> u64. Само число не значит ничего, кроме «изменилось»: подписчик перечитывает состав через B24Registry::list().

ЕДИНСТВЕННЫЙ ПОТРЕБИТЕЛЬ — цикл /ws/bitrix: без сигнала портал, подключённый на живом процессе (POST /api/bitrix/portals), молчал бы в уже открытой панели до переподключения сокета. watch, а не broadcast: важно текущее состояние состава, а не история, и три правки подряд можно схлопнуть в один пересбор; отставания у watch не бывает, то есть нечему давать Lagged.

Рядом появился B24Registry::shutdown (CancellationToken), отменяемый в shutdown_all: сокету нужен признак «останавливается сервер», а токен отдельного слота отменяется ещё и при отвязке ОДНОГО портала.
