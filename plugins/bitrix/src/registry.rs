//! Реестр порталов Bitrix24 — упорядоченный список живых [`B24State`].
//!
//! # Зачем он существует
//!
//! До мультипортальности плагин держал ровно одно состояние: один вебхук —
//! один портал — один троттлер. Реестр не меняет эту арифметику, он её
//! ТИРАЖИРУЕТ: каждый портал получает СВОЙ [`B24State`], а значит свой
//! API-клиент (⇒ свой бюджет 2 rps, который портал считает на пару
//! портал+IP), свой broadcast-канал, свой курсор событий и свой набор
//! воркеров. Общего между порталами — только общие настройки из корня
//! `bitrix.toml`, и те раздаются КОПИЕЙ
//! ([`BitrixRootConfig::portal_configs`]).
//!
//! Именно поэтому весь код под `B24State` остался нетронутым: он никогда не
//! знал, что портал в мире один.
//!
//! # Порядок — это порядок вкладок
//!
//! Список упорядочен, и порядок живёт в файле (массив `[[portals]]`). Иначе
//! перетаскивание вкладок забывалось бы при каждом перезапуске devforge.
//! Перестановка в памяти и в файле идёт по ОДНОМУ правилу
//! ([`crate::config::reorder_by_ids`]) — разойдись они, вкладки после
//! рестарта встали бы иначе, чем их только что перетащили.
//!
//! # Инвариант: список НИКОГДА не пуст
//!
//! Свежая установка (нет ни `[[portals]]`, нет и legacy-ключа `webhook`) даёт
//! ОДИН слот с ПУСТЫМ вебхуком и пустым `id` — состояние `not_configured` и
//! экран подключения, ровно как до мультипортальности. Без этого инварианта
//! пришлось бы городить второй, «безпортальный» режим роутов: у панели нет
//! состояния, в котором ей нечего показать.
//!
//! Пустой слот — не портал, а МЕСТО для портала:
//!
//! - [`B24Registry::get`] по пустому `id` не резолвится (пустой
//!   идентификатор не адресует ничего);
//! - [`B24Registry::connected`] его не считает — на нём держится «сколько у
//!   меня подключено порталов» во фронте;
//! - [`B24Registry::add`] его ЗАНИМАЕТ, а не добавляет рядом. Иначе первый же
//!   портал, добавленный новым роутом, дал бы вкладку-призрак и второй
//!   комплект воркеров.
//!
//! Симметрично: [`B24Registry::remove`] последнего портала возвращает пустой
//! слот на место — панель снова показывает экран подключения, как после
//! logout сегодня.
//!
//! # Состав объявляется наружу сигналом
//!
//! Каждое изменение состава (`add`, `remove`, `rename`, `reorder`) двигает
//! поколение в [`B24Registry::subscribe_composition`]. Ради одного
//! потребителя — сокета `/ws/bitrix`, который несёт кадры ВСЕХ порталов:
//! без сигнала портал, подключённый на живом процессе, молчал бы в уже
//! открытой панели до переподключения сокета (Ф5).
//!
//! # Что здесь НЕ делается
//!
//! - **Сеть.** Построение реестра синхронно и сетевонезависимо ровно по той
//!   же причине, что и [`crate::init`]: хост зовёт его до биндинга порта.
//!   Проверка вебхука — работа воркеров.
//! - **Чтение `FORGE_BITRIX_WEBHOOK`.** Реестр строится ИСКЛЮЧИТЕЛЬНО из
//!   [`BitrixRootConfig`], а переменную читает `load()`. Спроси реестр
//!   переменную сам — тестовый стенд смешался бы с живыми порталами
//!   пользователя, и тест сходил бы в боевой портал.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;

use crate::config::{self, BitrixConfig, BitrixRootConfig, WebhookUrl};
use crate::state::B24State;

/// Упорядоченный список порталов и их жизненный цикл.
///
/// Читается на КАЖДЫЙ HTTP-запрос (экстрактор портала, Ф3), поэтому чтение
/// сделано синхронным и не пересекается с воркерами: `std::sync::RwLock` без
/// единого `await` внутри критической секции.
pub struct B24Registry {
    /// Общие настройки и путь конфига. Пер-портальные вебхуки живут в слотах.
    root: BitrixRootConfig,

    /// Слоты в порядке вкладок. НИКОГДА не пуст (см. шапку модуля).
    slots: std::sync::RwLock<Vec<Arc<B24State>>>,

    /// Сериализует изменяющие операции.
    ///
    /// Все они устроены как read-modify-write НАД ФАЙЛОМ, а два таких
    /// одновременно — это потерянный портал: второй пишет от списка, который
    /// первый уже успел изменить.
    mutate: Mutex<()>,

    /// Были ли уже запущены воркеры ([`crate::spawn_workers`]).
    ///
    /// Нужен [`Self::add`]: портал, добавленный в РАБОТАЮЩЕМ процессе,
    /// обязан получить воркеров сразу, а собранный до старта — дождаться
    /// общего запуска, иначе получил бы их дважды.
    started: AtomicBool,

    /// Прошёл ли [`crate::shutdown`].
    ///
    /// Портал, добавленный после остановки, воркеров уже не получает: они
    /// стартовали бы под отменённым токеном и умерли на первом же `select!`,
    /// оставив вместо себя запись в логе об ошибке, которой не было.
    stopped: AtomicBool,

    /// Поколение СОСТАВА реестра: растёт на каждое `add`/`remove`/`rename`/
    /// `reorder`.
    ///
    /// # Зачем понадобился сигнал (Ф5)
    ///
    /// `/ws/bitrix` — ОДИН сокет на все порталы, и подписывается он на
    /// broadcast каждого слота в момент апгрейда. Портал, подключённый позже
    /// (`POST /api/bitrix/portals` на живом процессе), в этот набор не попал бы
    /// — и молчал бы в уже открытой панели до ручного переподключения сокета.
    /// Здесь сокет узнаёт, что набор пора пересобрать.
    ///
    /// `watch`, а не `broadcast`: подписчику важно ТЕКУЩЕЕ состояние состава, а
    /// не история изменений, и три правки подряд он имеет полное право
    /// схлопнуть в один пересбор. Отставания у `watch` не бывает по построению,
    /// то есть здесь нечему давать `Lagged` и нечего ресинхронизировать.
    ///
    /// Само число наружу не значит ничего, кроме «изменилось»: состав читается
    /// [`Self::list`], а не из этого канала.
    composition: watch::Sender<u64>,

    /// Общий сигнал остановки — ВСЕГО плагина, а не одного портала.
    ///
    /// У каждого [`B24State`] есть свой `shutdown`, и он отменяется в двух
    /// разных случаях: остановка процесса ([`Self::shutdown_all`]) и отвязка
    /// ОДНОГО портала ([`Self::remove`]). Сокету Ф5 нужен ровно первый: он
    /// переживает удаление любого портала (остальные продолжают вещать) и
    /// обязан закрыться только вместе с сервером. Токен слота такой разницы не
    /// различает, поэтому она живёт здесь.
    pub shutdown: CancellationToken,
}

impl B24Registry {
    /// Собирает реестр из корневого конфига.
    ///
    /// **Синхронна и сетевонезависима.** Порталов нет ⇒ один пустой слот (см.
    /// шапку модуля).
    ///
    /// # Errors
    ///
    /// Невалидные общие настройки (см. [`BitrixConfig::validate`]). Хост
    /// трактует это как «стартовать без плагина», а не как фатальный сбой.
    pub fn new(root: BitrixRootConfig) -> anyhow::Result<Arc<Self>> {
        // Перенос legacy-раскладки — ДО поднятия состояний: воркеры портала не
        // должны увидеть полупереехавший кэш, а курсор событий обязан лежать на
        // новом месте к моменту, когда его прочитают (Ф6.4).
        crate::migrate::migrate_legacy_layout(&root);

        let cfgs = root.portal_configs();
        let slots: Vec<Arc<B24State>> = if cfgs.is_empty() {
            vec![build_state(root.primary())?]
        } else {
            cfgs.into_iter()
                .map(build_state)
                .collect::<anyhow::Result<Vec<_>>>()?
        };

        tracing::info!(
            target: "forge_bitrix",
            portals = slots.len(),
            connected = slots.iter().filter(|s| !s.config.webhook.is_empty()).count(),
            ids = %slots.iter().map(|s| s.portal_id()).collect::<Vec<_>>().join(","),
            config_path = %root.shared.config_path.display(),
            "forge-bitrix: portal registry built"
        );

        Ok(Arc::new(Self {
            root,
            slots: std::sync::RwLock::new(slots),
            mutate: Mutex::new(()),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            composition: watch::channel(0).0,
            shutdown: CancellationToken::new(),
        }))
    }

    /// Реестр поверх УЖЕ поднятых состояний.
    ///
    /// Нужен там, где состояние собирается не из файла: тесты роутов (портал
    /// подменён стендом на `127.0.0.1`), интеграционные стенды и однопортальные
    /// точки входа, которым реестр нужен лишь как носитель `Arc<B24State>` для
    /// `Router::with_state`.
    ///
    /// Общие настройки берутся из конфига ПЕРВОГО слота: `BitrixConfig`
    /// остаётся пер-портальной структурой, а всё, что в ней общего (тайминги,
    /// лимиты, `config_path`), у слотов и так одинаково — их раздаёт
    /// [`BitrixRootConfig::portal_configs`] копией.
    ///
    /// `[[portals]]` при этом ПУСТ: секрета в файле нет, и записывать его
    /// отсюда нельзя. [`Self::add`] и [`Self::remove`] на таком реестре
    /// работают штатно — они читают и пишут файл по `config_path`, а не по этому
    /// полю.
    ///
    /// # Panics
    ///
    /// Пустой список слотов: реестр без слотов — состояние, которого нет (см.
    /// шапку модуля), и молча создавать его в тестовой фикстуре хуже, чем
    /// упасть на месте.
    pub fn from_states(slots: Vec<Arc<B24State>>) -> Arc<Self> {
        let shared = slots
            .first()
            .expect("реестр порталов никогда не бывает пустым")
            .config
            .clone();
        Arc::new(Self {
            root: BitrixRootConfig {
                shared,
                portals: Vec::new(),
                migrated_from_legacy: false,
            },
            slots: std::sync::RwLock::new(slots),
            mutate: Mutex::new(()),
            started: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            composition: watch::channel(0).0,
            shutdown: CancellationToken::new(),
        })
    }

    /// Реестр из ОДНОГО готового состояния — самая частая форма
    /// [`Self::from_states`].
    pub fn single(state: Arc<B24State>) -> Arc<Self> {
        Self::from_states(vec![state])
    }

    // -- чтение ------------------------------------------------------------

    /// Все слоты в порядке вкладок, включая ещё не подключённый.
    pub fn list(&self) -> Vec<Arc<B24State>> {
        self.read_slots()
    }

    /// Только подключённые порталы (у слота есть вебхук в КОНФИГЕ).
    ///
    /// Смотрит на `config.webhook`, а не на рантаймовый: метод синхронный,
    /// потому что его зовут из проекции DTO на каждой вкладке, а рантаймовый
    /// вебхук лежит под `tokio::RwLock`. Расходятся они ровно в одном случае —
    /// пустой слот, которому вебхук вставили из UI старым экраном входа; до
    /// перезапуска такой слот числится неподключённым, и это честно: в файле
    /// он появится порталом только после записи.
    pub fn connected(&self) -> Vec<Arc<B24State>> {
        self.read_slots()
            .into_iter()
            .filter(|s| !s.config.webhook.is_empty())
            .collect()
    }

    /// Портал по идентификатору. Не создаёт порталов и не ходит в сеть.
    ///
    /// Пустой `id` не резолвится: он адресует «слот без портала», а такой
    /// адресации у API нет.
    pub fn get(&self, id: &str) -> Option<Arc<B24State>> {
        if id.is_empty() {
            return None;
        }
        self.read_slots().into_iter().find(|s| s.portal_id() == id)
    }

    /// Первый слот — тот, что видит однопортальный код и старый экран входа.
    ///
    /// Не `Option`: список никогда не пуст (см. шапку модуля).
    pub fn primary(&self) -> Arc<B24State> {
        self.read_slots()
            .into_iter()
            .next()
            .expect("реестр порталов никогда не бывает пустым")
    }

    /// Сколько слотов в реестре (включая ещё не подключённый).
    pub fn len(&self) -> usize {
        self.read_slots().len()
    }

    /// Всегда `false` — список не бывает пустым; метод есть ради `clippy`.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Идентификаторы в порядке вкладок. У неподключённого слота — пустая
    /// строка.
    pub fn ids(&self) -> Vec<String> {
        self.read_slots().iter().map(|s| s.portal_id()).collect()
    }

    /// Общие настройки (тайминги, лимиты, пути). Вебхук в них — legacy-ключ.
    pub fn shared(&self) -> &BitrixConfig {
        &self.root.shared
    }

    /// Подписка на изменения СОСТАВА реестра.
    ///
    /// Значение канала — поколение ([`Self::composition_generation`]);
    /// подписчику полагается на `changed()` перечитать список
    /// ([`Self::list`]), а не пытаться понять что-то по самому числу.
    ///
    /// Подписываться нужно ДО чтения состава — ровно по той же причине, по
    /// которой WS подписывается на broadcast до снапшота: изменение,
    /// случившееся между чтением и подпиской, иначе потерялось бы навсегда.
    /// `watch` это прощает лишь наполовину — он хранит последнее значение, но
    /// не факт «оно менялось», — поэтому порядок здесь такой же строгий.
    pub fn subscribe_composition(&self) -> watch::Receiver<u64> {
        self.composition.subscribe()
    }

    /// Текущее поколение состава. Растёт монотонно; сравнивать имеет смысл
    /// только с ранее снятым значением.
    pub fn composition_generation(&self) -> u64 {
        *self.composition.borrow()
    }

    /// Объявляет, что состав изменился.
    ///
    /// `send_modify`, а не `send`: последний возвращает ошибку, когда живых
    /// подписчиков нет (панель закрыта), а «никто не слушает» — это норма, а
    /// не сбой добавления портала.
    fn bump_composition(&self, reason: &str) {
        self.composition.send_modify(|gen| *gen = gen.wrapping_add(1));
        tracing::debug!(
            target: "forge_bitrix",
            reason,
            generation = self.composition_generation(),
            "forge-bitrix: portal composition changed"
        );
    }

    /// Файл, из которого реестр прочитан и в который он пишет.
    ///
    /// Читаем и пишем ОДИН файл: иначе удаление портала стирало бы секрет не
    /// оттуда, откуда он был прочитан.
    pub fn config_path(&self) -> &Path {
        &self.root.shared.config_path
    }

    // -- жизненный цикл ----------------------------------------------------

    /// Запускает воркеров ВСЕХ слотов. Возвращается немедленно.
    ///
    /// Повторный вызов ничего не делает: воркеры регистрируются в состоянии
    /// портала и второй набор означал бы двойной опрос портала на один бюджет
    /// 2 rps.
    ///
    /// После этого вызова [`Self::add`] поднимает воркеров новому порталу сам —
    /// именно так портал подключается без рестарта devforge.
    pub async fn spawn_all(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        for slot in self.read_slots() {
            spawn_portal_workers(&slot).await;
        }
        tracing::info!(
            target: "forge_bitrix",
            portals = self.len(),
            "forge-bitrix: workers spawned for every portal"
        );
    }

    /// Гасит ВСЕ порталы полным graceful-путём.
    ///
    /// **Идемпотентна**: хост зовёт её из обработчика сигнала, а тот
    /// срабатывает и на Ctrl-C, и на SIGTERM от systemd.
    pub async fn shutdown_all(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Первым делом — общий токен: он будит WS-циклы, которые иначе
        // досыпали бы до heartbeat'а, держа сервер за порт.
        self.shutdown.cancel();
        tracing::info!(target: "forge_bitrix", "forge-bitrix: shutdown initiated");
        for slot in self.read_slots() {
            shutdown_portal(&slot).await;
        }
        tracing::info!(target: "forge_bitrix", "forge-bitrix: shutdown complete");
    }

    // -- добавление портала ------------------------------------------------

    /// Подключает портал по вебхуку — БЕЗ рестарта devforge.
    ///
    /// Возвращает состояние портала: новое либо уже существовавшее.
    /// Идемпотентна по секрету — тот же вебхук не заводит второй портал, а
    /// лишь обновляет `title`. Это не косметика: два состояния на один секрет
    /// означали бы два независимых троттлера, то есть двойной бюджет 2 rps на
    /// пару портал+IP, а портал такой избыток не отвергает ошибкой — он рвёт
    /// TCP-соединение.
    ///
    /// # Порядок шагов
    ///
    /// 1. Секрет ложится в файл ([`crate::config::add_portal_to`], там же
    ///    выводится окончательный `id` с разрешением коллизий);
    /// 2. поднимается состояние; провалилось — запись из файла СНИМАЕТСЯ, и
    ///    наружу уходит ошибка;
    /// 3. запускаются воркеры;
    /// 4. и только теперь портал публикуется в списке.
    ///
    /// Публикация последней — ради требования «ошибка не оставляет полуживого
    /// портала»: всё, что видно снаружи, уже работает.
    ///
    /// # Пустой слот
    ///
    /// Свежая установка держит один слот без вебхука. Первый `add` ЗАНИМАЕТ
    /// его, а не встаёт рядом: иначе рядом с рабочим порталом навсегда осталась
    /// бы вкладка-призрак, да ещё со вторым комплектом воркеров. Воркеры
    /// пустого слота уже крутятся и ждут подключения на
    /// `authorized_notify` — ровно поэтому занять слот можно на живом
    /// процессе.
    ///
    /// # Errors
    ///
    /// Пустой вебхук; ошибки записи конфига; невалидная конфигурация портала.
    pub async fn add(
        &self,
        webhook: &WebhookUrl,
        title: Option<&str>,
    ) -> anyhow::Result<Arc<B24State>> {
        if webhook.is_empty() {
            anyhow::bail!("the webhook must not be empty");
        }
        let _guard = self.mutate.lock().await;

        // Один и тот же секрет ⇒ один и тот же портал.
        if let Some(state) = self.slot_with_webhook(webhook).await {
            let saved = config::add_portal_to(self.config_path(), webhook, title)?;
            // Слот с секретом, но БЕЗ идентификатора — это всё тот же пустой
            // слот, которому вебхук вставили в обход `add` (старый экран
            // входа). Он тоже обязан пересобраться, иначе портал останется с
            // каталогом `_vacant` (см. [`Self::occupy_vacant_slot`]).
            if state.portal_id().is_empty() {
                return Ok(self.occupy_vacant_slot(&state, &saved, webhook).await);
            }
            state.set_portal_identity(saved.id.clone(), saved.title.clone());
            // Состав списка не изменился, но ПОДПИСЬ вкладки — да, а строку
            // вкладок рисует тот же кадр `portals`: без сигнала повторное
            // добавление с новым именем меняло бы его только после F5.
            self.bump_composition("title updated");
            tracing::info!(
                target: "forge_bitrix",
                portal = %saved.id,
                "forge-bitrix: the portal is already connected; only its title was updated"
            );
            return Ok(state);
        }

        let saved = config::add_portal_to(self.config_path(), webhook, title)?;

        // Пустой слот занимаем, а не дополняем.
        if let Some(slot) = self.vacant_slot().await {
            let state = self.occupy_vacant_slot(&slot, &saved, webhook).await;
            tracing::info!(
                target: "forge_bitrix",
                portal = %saved.id,
                webhook = %webhook.fingerprint(),
                cache_dir = %state.config.cache_dir.display(),
                "forge-bitrix: the vacant slot now holds a portal"
            );
            return Ok(state);
        }

        let cfg = portal_cfg(&self.root.shared, &saved.id, &saved.title, webhook);
        let state = match build_state(cfg) {
            Ok(state) => state,
            Err(e) => {
                // Полуживого портала быть не должно ни в списке, ни в файле.
                if let Err(undo) = config::remove_portal_from(self.config_path(), &saved.id) {
                    tracing::error!(
                        target: "forge_bitrix",
                        portal = %saved.id,
                        error = %undo,
                        "forge-bitrix: could not roll back the portal record"
                    );
                }
                return Err(e);
            }
        };

        // Воркеры — ДО публикации: всё, что видно снаружи, уже работает.
        if self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst) {
            spawn_portal_workers(&state).await;
        }
        self.write_slots().push(state.clone());
        // Сигнал — ПОСЛЕ публикации: сокет по нему перечитывает список, и
        // разбуженный раньше он не увидел бы новичка.
        self.bump_composition("portal added");

        tracing::info!(
            target: "forge_bitrix",
            portal = %saved.id,
            webhook = %webhook.fingerprint(),
            portals = self.len(),
            "forge-bitrix: portal added at runtime"
        );
        Ok(state)
    }

    /// Ставит портал на место пустого слота, ПЕРЕСОБИРАЯ его состояние.
    ///
    /// # Почему пересборка, а не переименование на месте
    ///
    /// До Ф6 хватало двух строк: вставить вебхук и выдать слоту личность
    /// (`set_webhook` + `set_portal_identity`). С Ф6 этого мало. Дисковые пути
    /// портала выводятся из его идентификатора и живут в `BitrixConfig`, а тот
    /// НЕИЗМЕНЯЕМ (`B24State` раздаётся как `Arc`). Слот, стартовавший пустым,
    /// собран с каталогом `_vacant`, и, получив личность без пересборки, он
    /// продолжил бы писать медиа, staging, курсор событий и заметки анализа
    /// туда же. Следующий запуск построил бы этот портал уже с каталогом
    /// `<кэш>/<id>/` — и не нашёл бы ни скачанных вложений, ни КУРСОРА
    /// СОБЫТИЙ, а `im.v2.Event.*` ретроспективы не имеет: пропущенное за время
    /// холодного старта не вернётся.
    ///
    /// Хранить путь отдельной изменяемой ячейкой (рядом с `portal`) было бы
    /// половиной решения: `AnalysisHub` получает путь к заметкам КОПИЕЙ при
    /// сборке состояния, и переставить его задним числом всё равно нельзя.
    /// Пересборка чинит оба случая одним движением.
    ///
    /// # Что переносится в новое состояние
    ///
    /// Состояние автомата подключения — и только оно. Соблазн оставить новому
    /// слоту его собственный `disconnected` («вебхук есть, но не проверен»)
    /// велик, но занятый слот — это ЖИВОЙ экран подключения: сокет на смену
    /// состава шлёт `login_state` нового слота, и пользователь, только что
    /// вставивший вебхук, увидел бы вспышку «нет связи» перед «проверяем».
    /// Автомат двигает проверка, которую делает вызывающий (роуты Ф4) и которая
    /// начинается с `checking`; `add` обязан оставаться дешёвым и молчаливым.
    ///
    /// Больше переносить нечего: у слота без вебхука нет ни API-клиента, ни
    /// курсора, ни кэша собеседников — он всю свою жизнь ждал подключения.
    ///
    /// # Порядок шагов
    ///
    /// Сначала подмена в списке, потом гашение старого слота — тот же порядок,
    /// что в [`Self::remove`]: новые запросы перестают попадать на состояние
    /// ДО того, как оно начнёт исчезать. Сокет переживает подмену сам —
    /// [`crate::ws::PortalFanIn::sync`] сверяет подписки по `Arc::ptr_eq`,
    /// поэтому по сигналу состава он отпишется от старого слота и подпишется на
    /// новый.
    ///
    /// # Отказа нет
    ///
    /// Занятие слота не может провалиться: общие настройки уже прошли
    /// валидацию при сборке реестра (см. [`state_from_cfg`]), а вебхук проверил
    /// вызывающий. Это же и историческое поведение — занятие слота никогда
    /// ничего не валидировало.
    async fn occupy_vacant_slot(
        &self,
        slot: &Arc<B24State>,
        saved: &config::PortalConfig,
        webhook: &WebhookUrl,
    ) -> Arc<B24State> {
        let cfg = portal_cfg(&self.root.shared, &saved.id, &saved.title, webhook);
        let state = state_from_cfg(cfg);

        {
            let carried = slot.login.lock().await.clone();
            *state.login.lock().await = carried;
        }

        {
            let mut slots = self.write_slots();
            match slots.iter().position(|s| Arc::ptr_eq(s, slot)) {
                Some(i) => slots[i] = state.clone(),
                // Слот успели снять другим путём: публикуем портал обычным
                // порядком, вместо того чтобы потерять его совсем.
                None => slots.push(state.clone()),
            }
        }

        shutdown_portal(slot).await;

        if self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst) {
            spawn_portal_workers(&state).await;
        }

        // Слот тот же по смыслу, но НАЗЫВАТЬСЯ он стал: до этого момента его не
        // было ни в одной вкладке (пустой `id` отфильтрован), после — есть.
        self.bump_composition("vacant slot occupied");
        state
    }

    /// Слот с ЭТИМ секретом, если он уже есть.
    ///
    /// Сравнивает рантаймовый вебхук, а не конфиговый: пустой слот, которому
    /// вебхук вставили старым экраном входа, в конфиге числится пустым — и без
    /// этой проверки `add` того же URL завёл бы второй троттлер на тот же
    /// секрет.
    async fn slot_with_webhook(&self, webhook: &WebhookUrl) -> Option<Arc<B24State>> {
        for slot in self.read_slots() {
            if slot.webhook().await == *webhook {
                return Some(slot);
            }
        }
        None
    }

    /// Единственный слот без вебхука — «место для портала».
    ///
    /// Только когда он ОДИН и он же весь реестр: пустой слот рядом с рабочим
    /// порталом — состояние, которого реестр не допускает, и наткнувшись на
    /// него не следует делать вид, что всё в порядке.
    async fn vacant_slot(&self) -> Option<Arc<B24State>> {
        let slots = self.read_slots();
        if slots.len() != 1 {
            return None;
        }
        let only = slots.into_iter().next()?;
        (!only.has_webhook().await).then_some(only)
    }

    // -- удаление портала --------------------------------------------------

    /// Отключает портал: гасит его воркеров, стирает секрет и (по флагу) кэш.
    ///
    /// Возвращает `false`, если портала с таким `id` в реестре нет. Это НЕ
    /// ошибка: повторный клик по «отключить» не должен превращаться в красный
    /// тост, а роут Ф4 сам решит, отвечать ли 404.
    ///
    /// # Порядок шагов и почему именно такой
    ///
    /// 1. **Снять со списка.** Новые запросы к порталу перестают резолвиться
    ///    ДО того, как под ними начнёт исчезать состояние;
    /// 2. **погасить** полным путём [`shutdown_portal`] — `cancel()` +
    ///    `shutdown_workers()` (две секунды на самостоятельный выход: воркер
    ///    событий на выходе персистит курсор) + `analysis.shutdown()` (иначе
    ///    процесс `claude` пережил бы удаление вкладки и продолжил бы жечь
    ///    токены осиротевшим);
    /// 3. **стереть секрет** через [`crate::config::remove_portal_from`] — со
    ///    всеми четырьмя инвариантами записи конфига;
    /// 4. по флагу — снести каталог кэша портала.
    ///
    /// Гасим ПОСЛЕ снятия со списка и стираем секрет ПОСЛЕ гашения: упади
    /// процесс между шагами, на диске останется портал, который просто
    /// поднимется заново, а не полуудалённая запись без состояния.
    ///
    /// # Удаление последнего портала
    ///
    /// На его место встаёт пустой слот — панель снова показывает экран
    /// подключения, ровно как после logout сегодня (см. инвариант «список
    /// никогда не пуст» в шапке модуля).
    ///
    /// # `purge_cache`
    ///
    /// Сносится каталог, ОТЛИЧНЫЙ от общего `cache_dir` и лежащий внутри него.
    /// До Ф6 пер-портальных подкаталогов ещё нет, все порталы делят один
    /// каталог — и снести его значило бы стереть кэш соседей. Проверка
    /// сформулирована так, что после Ф6 (когда `cache_dir` станет
    /// пер-портальным) она начнёт работать сама, без правки этого места.
    ///
    /// # Errors
    ///
    /// Ошибки записи конфига. Неудачное удаление кэша ошибкой НЕ считается:
    /// портал уже отключён, а место на диске — не повод откатывать отключение.
    pub async fn remove(&self, id: &str, purge_cache: bool) -> anyhow::Result<bool> {
        let _guard = self.mutate.lock().await;

        let removed = {
            let mut slots = self.write_slots();
            match slots.iter().position(|s| s.portal_id() == id) {
                Some(i) => slots.remove(i),
                None => return Ok(false),
            }
        };

        // Список не бывает пустым: на место последнего портала встаёт слот.
        let vacant = if self.read_slots().is_empty() {
            let cfg = portal_cfg(&self.root.shared, "", "", &WebhookUrl::default());
            let slot = build_state(cfg.clone()).unwrap_or_else(|e| {
                // Общие настройки уже проходили валидацию при сборке реестра,
                // так что сюда попасть можно разве что после правки файла
                // руками. Пустой реестр всё равно недопустим — и НАСТОЛЬКО
                // недопустим, что невалидность настроек его не оправдывает:
                // без слота экстрактор портала начинает отвечать 503 на КАЖДЫЙ
                // запрос, включая `/status`, то есть панель не может даже
                // показать экран подключения, чтобы пользователь исправил
                // положение. Слоту при этом валидность и не нужна: он ничего
                // не делает, пока в него не вставили вебхук, а `add` пересоберёт
                // конфигурацию сам.
                tracing::error!(
                    target: "forge_bitrix",
                    error = %e,
                    "forge-bitrix: the shared config is invalid; the vacant slot is restored as is"
                );
                B24State::new(cfg)
            });
            self.write_slots().push(slot.clone());
            Some(slot)
        } else {
            None
        };

        shutdown_portal(&removed).await;
        config::remove_portal_from(self.config_path(), id)?;

        if purge_cache {
            purge_portal_cache(&removed.config, &self.root.shared);
        }

        // Экран подключения обязан ожить без рестарта — значит и его воркеры
        // тоже.
        if let Some(slot) = vacant {
            if self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst) {
                spawn_portal_workers(&slot).await;
            }
        }

        self.bump_composition("portal removed");
        tracing::info!(
            target: "forge_bitrix",
            portal = %id,
            purged_cache = purge_cache,
            portals = self.len(),
            "forge-bitrix: portal removed (the portal-side webhook is still valid)"
        );
        Ok(true)
    }

    // -- переименование и порядок -----------------------------------------

    /// Меняет отображаемое имя портала. Секрет и `id` не трогает.
    ///
    /// Возвращает `false`, если портала с таким `id` нет — ни в реестре, ни в
    /// файле. Пустое имя означает «вернуть автоматическую подпись» (хост
    /// портала).
    ///
    /// Файл правится ПЕРВЫМ: провались запись — в памяти осталось бы имя,
    /// которого нет на диске, и переименование «отменялось» бы само собой при
    /// следующем запуске.
    ///
    /// # Errors
    ///
    /// Ошибки чтения/записи конфига.
    pub async fn rename(&self, id: &str, title: &str) -> anyhow::Result<bool> {
        let _guard = self.mutate.lock().await;
        let Some(state) = self.get(id) else {
            return Ok(false);
        };
        if !config::rename_portal_in(self.config_path(), id, title)? {
            return Ok(false);
        }
        state.set_portal_title(title);
        self.bump_composition("portal renamed");
        tracing::info!(
            target: "forge_bitrix",
            portal = %id,
            "forge-bitrix: portal renamed"
        );
        Ok(true)
    }

    /// Переставляет вкладки в порядке `ids` — и в памяти, и в файле.
    ///
    /// Устойчива к неполному и грязному списку: неизвестные `id`
    /// игнорируются, не перечисленные слоты уезжают в хвост с сохранением
    /// относительного порядка. Ни один портал не теряется — список приходит из
    /// фронта, а фронт мог отстать от состава реестра на один запрос.
    ///
    /// Файл правится первым, по той же причине, что и в [`Self::rename`].
    ///
    /// # Errors
    ///
    /// Ошибки чтения/записи конфига.
    pub async fn reorder(&self, ids: &[String]) -> anyhow::Result<()> {
        let _guard = self.mutate.lock().await;
        config::reorder_portals_in(self.config_path(), ids)?;
        {
            let mut slots = self.write_slots();
            let current = std::mem::take(&mut *slots);
            *slots = config::reorder_by_ids(current, ids, |s| s.portal_id());
        }
        self.bump_composition("portals reordered");
        tracing::info!(
            target: "forge_bitrix",
            order = %self.ids().join(","),
            "forge-bitrix: portals reordered"
        );
        Ok(())
    }

    // -- внутреннее --------------------------------------------------------

    /// Снимок списка. Терпит отравленный лок: состав вкладок — не инвариант
    /// безопасности, а ронять из-за него каждый последующий запрос незачем.
    fn read_slots(&self) -> Vec<Arc<B24State>> {
        match self.slots.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn write_slots(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Arc<B24State>>> {
        match self.slots.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Поднимает состояние одного портала из его конфигурации.
///
/// Ровно та же последовательность, что была в однопортальном `init`:
/// валидация → подготовка каталогов (не фатальна) → [`B24State::new`].
///
/// # Errors
///
/// Невалидная конфигурация (см. [`BitrixConfig::validate`]).
fn build_state(cfg: BitrixConfig) -> anyhow::Result<Arc<B24State>> {
    cfg.validate()?;
    Ok(state_from_cfg(cfg))
}

/// То же БЕЗ повторной валидации — для занятия пустого слота.
///
/// Валидация проверяет ОБЩИЕ настройки (тайминги, лимиты, правило Р17 о
/// `base_url_override`), а они у занимаемого слота уже прошли её при сборке
/// реестра: [`BitrixConfig::for_portal`] меняет только вебхук, личность и
/// выведенные из неё пути. Прогнать её здесь ещё раз означало бы завернуть
/// подключение первого портала на стенде `http://127.0.0.1` — том самом, ради
/// которого правило Р17 и делает исключение по `FORGE_BITRIX_CONFIG_PATH`.
/// Исторически занятие слота не валидировало ничего, и это поведение
/// сохраняется.
fn state_from_cfg(cfg: BitrixConfig) -> Arc<B24State> {
    if let Err(e) = cfg.ensure_dirs() {
        // Не фатально: каталог понадобится только при записи кэша, и там
        // ошибка всплывёт с более точным контекстом.
        tracing::warn!(
            target: "forge_bitrix",
            error = %e,
            portal = %cfg.portal_id,
            "forge-bitrix: could not prepare the cache directory"
        );
    }
    let state = B24State::new(cfg);
    tracing::info!(
        target: "forge_bitrix",
        portal = %state.portal_id(),
        login_state = if state.config.has_webhook() { "disconnected" } else { "not_configured" },
        rate_limit_rps = state.config.rate_limit_rps,
        poll_active_secs = state.config.poll_active_secs,
        poll_idle_secs = state.config.poll_idle_secs,
        recent_sync_secs = state.config.recent_sync_secs,
        cache_dir = %state.config.cache_dir.display(),
        webhook = %state.config.webhook.fingerprint(),
        "forge-bitrix: portal state initialized"
    );
    state
}

/// Конфигурация для нового портала: общие настройки КОПИЕЙ плюс его личность.
///
/// Копией — потому что `BitrixConfig` остаётся ПЕР-ПОРТАЛЬНОЙ структурой:
/// именно на этом держится неизменность `B24State::new(cfg)` и всего кода под
/// ней.
fn portal_cfg(shared: &BitrixConfig, id: &str, title: &str, webhook: &WebhookUrl) -> BitrixConfig {
    shared.for_portal(id, title, webhook)
}

/// Запускает полный набор воркеров ОДНОГО портала.
///
/// Состав и причины — в шапке [`crate::spawn_workers`]. Здесь важно одно:
/// набор ОДИН И ТОТ ЖЕ и для порталов из конфига, и для добавленного в
/// рантайме. Разойдись эти два пути — портал, подключённый без рестарта,
/// молча остался бы, например, без подчистки кэша, и заметили бы это через
/// неделю по забитому диску.
///
/// Возвращается немедленно: воркеры именно ЗАПУСКАЮТСЯ, а не выполняются.
pub async fn spawn_portal_workers(state: &Arc<B24State>) {
    let configured = state.has_webhook().await;
    if configured {
        let st = state.clone();
        let handle = tokio::spawn(async move {
            tokio::select! {
                // Остановка сервера не должна ждать connect-таймаута портала.
                _ = st.shutdown.cancelled() => {}
                flow = crate::login::verify_saved_webhook(&st) => {
                    tracing::info!(
                        target: "forge_bitrix",
                        portal = %st.portal_id(),
                        state = flow.kind(),
                        "forge-bitrix: startup webhook check finished"
                    );
                }
            }
        });
        state.register_worker(handle).await;
    }
    crate::events::spawn(state).await;
    crate::recent_sync::spawn(state).await;
    // Подчистка кэша НЕ привязана к подключению: скачанные медиа переживают
    // logout, и без своего воркера подметать их было бы некому.
    crate::media_cache::spawn_sweeper(state).await;
    // GC незавершённых загрузок (Ф9). Первый прогон — сразу на старте: реестр
    // живёт в памяти, поэтому после рестарта каждый `.part` в
    // `<cache_dir>/uploads/` — сирота, на которую уже никто не сошлётся, а
    // место она занимает вполне реальное.
    crate::routes::upload::spawn_uploads_gc(state).await;
    tracing::info!(
        target: "forge_bitrix",
        portal = %state.portal_id(),
        configured,
        workers = usize::from(configured) + 4,
        "forge-bitrix: portal workers spawned"
    );
}

/// Сносит каталог кэша отключённого портала — и только его.
///
/// Два условия, и оба обязательны:
///
/// - каталог ОТЛИЧЕН от общего `cache_dir` — до Ф6 все порталы делят один
///   каталог, и снести его значило бы стереть кэш соседей;
/// - каталог лежит ВНУТРИ общего — правка `cache_dir` руками не должна
///   превращать отключение вкладки в удаление произвольной папки.
///
/// Ошибка не всплывает наружу: портал уже отключён, и место на диске — не
/// повод откатывать отключение. Она попадает в лог, где ей и место.
fn purge_portal_cache(portal: &BitrixConfig, shared: &BitrixConfig) {
    let dir = &portal.cache_dir;
    if dir == &shared.cache_dir || !dir.starts_with(&shared.cache_dir) {
        tracing::info!(
            target: "forge_bitrix",
            dir = %dir.display(),
            "forge-bitrix: the cache is shared with the other portals; keeping it"
        );
        return;
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => tracing::info!(
            target: "forge_bitrix",
            dir = %dir.display(),
            "forge-bitrix: the portal cache directory was removed"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            target: "forge_bitrix",
            dir = %dir.display(),
            error = %e,
            "forge-bitrix: could not remove the portal cache directory"
        ),
    }
}

/// Гасит ОДИН портал полным graceful-путём.
///
/// 1. `shutdown.cancel()` — будит WS-циклы и воркеров;
/// 2. `shutdown_workers()` — даёт каждому две секунды выйти самому и только
///    потом абортит. Пауза не косметическая: воркер событий на выходе
///    персистит курсор, а `abort()` в произвольной await-точке этого сделать
///    не даст;
/// 3. `analysis.shutdown()` — убивает процессы `claude`, запущенные анализом
///    чата. Без него дочерний процесс пережил бы devforge и продолжал бы жечь
///    токены осиротевшим.
///
/// **Идемпотентна**: `CancellationToken` можно отменять сколько угодно раз, а
/// список воркеров после первого прохода пуст.
///
/// Тот же путь проходит и портал, УДАЛЯЕМЫЙ из реестра ([`B24Registry::remove`]):
/// удаление вкладки обязано гасить процессы `claude` ровно так же, как это
/// делает остановка всего devforge.
pub async fn shutdown_portal(state: &Arc<B24State>) {
    tracing::info!(
        target: "forge_bitrix",
        portal = %state.portal_id(),
        "forge-bitrix: portal shutdown initiated"
    );
    state.shutdown.cancel();
    state.shutdown_workers().await;
    state.analysis.shutdown().await;
    tracing::info!(
        target: "forge_bitrix",
        portal = %state.portal_id(),
        "forge-bitrix: portal shutdown complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PortalConfig;

    /// Корневой конфиг с герметичными путями: без явных путей хранилище
    /// заметок легло бы в живой `~/.forge/...` разработчика, а CLI-заглушка
    /// гарантирует, что тест не запустит настоящий claude.
    fn test_root(dir: &Path, portals: &[(&str, &str)]) -> BitrixRootConfig {
        let mut shared = BitrixConfig {
            cache_dir: dir.join("cache"),
            prompts_path: dir.join("prompts.json"),
            claude_cli_path: dir.join("no-such-claude"),
            config_path: dir.join("bitrix.toml"),
            ..BitrixConfig::default()
        };
        // Заведомо закрытый порт: сходи что-нибудь в сеть — тест споткнулся бы
        // о connect-таймаут вместо мгновенного завершения.
        shared.base_url_override = "https://127.0.0.1:1/rest/1/abc123".into();
        BitrixRootConfig {
            shared,
            portals: portals
                .iter()
                .map(|(id, host)| PortalConfig {
                    id: (*id).to_string(),
                    title: String::new(),
                    webhook: WebhookUrl::new(format!("https://{host}/rest/1/abc123/")),
                })
                .collect(),
            migrated_from_legacy: false,
        }
    }

    #[test]
    fn the_list_keeps_the_config_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = B24Registry::new(test_root(
            dir.path(),
            &[("acme", "acme.bitrix24.ru"), ("beta", "beta.bitrix24.ru")],
        ))
        .unwrap();

        assert_eq!(reg.ids(), vec!["acme", "beta"]);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.connected().len(), 2);
        assert_eq!(reg.primary().portal_id(), "acme");
    }

    #[test]
    fn an_empty_config_still_yields_one_unconfigured_slot() {
        let dir = tempfile::tempdir().unwrap();
        let reg = B24Registry::new(test_root(dir.path(), &[])).unwrap();

        assert_eq!(reg.len(), 1, "экран подключения всегда есть на чём рисовать");
        assert!(reg.connected().is_empty(), "но портала ещё нет");
        assert_eq!(reg.primary().portal_id(), "");
        assert!(!reg.primary().config.has_webhook());
    }

    #[test]
    fn get_resolves_nothing_for_an_unknown_or_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        let reg = B24Registry::new(test_root(dir.path(), &[("acme", "acme.bitrix24.ru")])).unwrap();

        assert!(reg.get("acme").is_some());
        assert!(reg.get("nope").is_none());
        assert!(reg.get("").is_none(), "пустой id не адресует ничего");
    }

    /// Реестр, прочитанный из РЕАЛЬНОГО файла, но с герметичными путями.
    fn reg_from_file(dir: &Path) -> Arc<B24Registry> {
        let mut root = BitrixRootConfig::load_from_file(&dir.join("bitrix.toml"));
        root.shared.cache_dir = dir.join("cache");
        root.shared.prompts_path = dir.join("prompts.json");
        root.shared.claude_cli_path = dir.join("no-such-claude");
        root.shared.base_url_override = "https://127.0.0.1:1/rest/1/abc123".into();
        B24Registry::new(root).unwrap()
    }

    /// КРИТЕРИЙ Ф6.4 НА СТАРТЕ: legacy-конфиг с верхнеуровневым `webhook` и
    /// данными в корне кэша поднимается уже переехавшим.
    ///
    /// Отдельно от юнит-тестов [`crate::migrate`] потому, что проверяет СВЯЗКУ:
    /// перенос обязан случиться внутри `B24Registry::new`, до поднятия
    /// состояний. Забудь кто-нибудь этот вызов — юнит-тесты переноса остались
    /// бы зелёными, а пользователь при обновлении увидел бы пустой кэш и
    /// перечитанную с нуля очередь событий.
    #[tokio::test]
    async fn a_legacy_install_is_already_migrated_when_the_registry_is_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        // Ровно то, что оставляла однопортальная сборка: ключ верхнего уровня.
        std::fs::write(
            &path,
            "webhook = \"https://acme.bitrix24.ru/rest/1/legacyc0de/\"\n",
        )
        .unwrap();

        // И её данные — в КОРНЕ кэша.
        let cache = dir.path().join("cache");
        std::fs::create_dir_all(cache.join("media/chat7")).unwrap();
        std::fs::write(cache.join("media/chat7/113.0.file.bin"), b"attachment").unwrap();
        std::fs::write(cache.join("events.json"), b"{\"offset\":\"42\"}").unwrap();
        std::fs::write(dir.path().join("prompts.json"), b"{\"notes\":1}").unwrap();

        let reg = reg_from_file(dir.path());
        let portal = reg.primary();
        assert_eq!(portal.portal_id(), "acme", "legacy-ключ дал один портал");

        assert_eq!(
            std::fs::read(portal.config.cache_dir.join("media/chat7/113.0.file.bin")).unwrap(),
            b"attachment",
            "скачанное вложение пережило обновление"
        );
        assert_eq!(
            std::fs::read(&portal.config.prompts_path).unwrap(),
            b"{\"notes\":1}",
            "сделанные разборы пережили обновление"
        );
        // Курсор — самое дорогое из переносимого: не найдя его, портал
        // перечитал бы или потерял очередь, а ретроспективы у `im.v2.Event.*`
        // нет. Читается он ИМЕННО по новому пути.
        assert_eq!(
            crate::events::load_cursor(&crate::events::cursor_path(&portal.config), 0)
                .map(|c| c.offset),
            Some("42".to_string()),
            "курсор переехал и читается по новому пути"
        );
        assert!(
            !cache.join("media").exists() && !cache.join("events.json").exists(),
            "в корне не осталось legacy-данных"
        );

        reg.shutdown_all().await;
    }

    /// КРИТЕРИЙ Ф2.2: портал поднимается в РАБОТАЮЩЕМ процессе, получает
    /// собственный набор воркеров, свой broadcast и свой курсор, а сосед
    /// продолжает работать.
    #[tokio::test]
    async fn add_raises_a_second_portal_with_its_own_workers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let first = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/");
        config::save_portals_to(&path, &[PortalConfig::from_webhook(&first, None)]).unwrap();

        let reg = reg_from_file(dir.path());
        reg.spawn_all().await;
        let acme = reg.get("acme").unwrap();
        assert_eq!(acme.workers.lock().await.len(), 5);

        let second = WebhookUrl::new("https://beta.bitrix24.ru/rest/7/bbb/");
        let beta = reg.add(&second, Some("Бета")).await.unwrap();

        assert_eq!(reg.ids(), vec!["acme", "beta"], "порядок — порядок вкладок");
        assert_eq!(beta.portal_title(), "Бета");
        assert_eq!(
            beta.workers.lock().await.len(),
            5,
            "у нового портала свой полный набор воркеров"
        );
        assert_eq!(
            acme.workers.lock().await.len(),
            5,
            "первый портал продолжает работать"
        );

        // Свой broadcast и свой курсор: каналы разные, курсоры независимы.
        assert!(
            !std::sync::Arc::ptr_eq(&acme, &beta),
            "порталы — разные состояния"
        );
        assert!(beta.event_cursor().await.is_none());
        assert_eq!(acme.subscribe().len(), 0);

        // Секрет доехал до файла, чужой не тронут.
        let reread = BitrixRootConfig::load_from_file(&path);
        assert_eq!(reread.portal_ids(), vec!["acme".to_string(), "beta".into()]);
        assert_eq!(reread.portals[0].webhook.expose(), first.expose());
        assert_eq!(reread.portals[1].webhook.expose(), second.expose());

        reg.shutdown_all().await;
    }

    /// Тот же секрет второго портала не заводит: два состояния на один вебхук
    /// означали бы два троттлера на один бюджет 2 rps.
    #[tokio::test]
    async fn add_is_idempotent_by_secret_and_only_updates_the_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let w = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/");
        config::save_portals_to(&path, &[PortalConfig::from_webhook(&w, None)]).unwrap();

        let reg = reg_from_file(dir.path());
        let again = reg.add(&w, Some("Тот же")).await.unwrap();

        assert_eq!(reg.len(), 1);
        assert_eq!(again.portal_title(), "Тот же");
        assert_eq!(BitrixRootConfig::load_from_file(&path).portals.len(), 1);
    }

    /// Первый портал ЗАНИМАЕТ пустой слот, а не встаёт рядом с ним: иначе
    /// рядом с рабочей вкладкой навсегда осталась бы вкладка-призрак.
    ///
    /// «Занимает» с Ф6 означает ПЕРЕСБОРКУ состояния, а не переименование на
    /// месте: дисковые пути выведены из идентификатора и лежат в неизменяемом
    /// `BitrixConfig`. Слот, оставленный прежним, продолжил бы писать в
    /// каталог `_vacant` — и следующий запуск не нашёл бы ни вложений, ни
    /// курсора событий (см. [`B24Registry::occupy_vacant_slot`]). Поэтому
    /// проверяется не тождество `Arc`, а то, ради чего слот вообще занимают:
    /// вкладка одна, у неё есть идентификатор, и её каталог — портальный.
    #[tokio::test]
    async fn the_first_add_occupies_the_vacant_slot() {
        let dir = tempfile::tempdir().unwrap();
        let reg = reg_from_file(dir.path());
        reg.spawn_all().await;
        let vacant = reg.primary();
        assert_eq!(
            vacant.workers.lock().await.len(),
            4,
            "без вебхука стартовой проверки нет"
        );

        let w = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/");
        let portal = reg.add(&w, None).await.unwrap();

        assert_eq!(reg.len(), 1, "вкладка-призрак не появилась");
        assert_eq!(portal.portal_id(), "acme");
        assert_eq!(
            portal.config.cache_dir,
            vacant.config.cache_root.join("acme"),
            "занятый слот пишет в каталог ПОРТАЛА, а не в `_vacant`"
        );
        assert_eq!(
            portal.config.prompts_path,
            crate::config::portal_prompts_path(&vacant.config.prompts_root, "acme"),
            "и заметки его тоже — в файл портала"
        );
        assert!(
            !std::sync::Arc::ptr_eq(&vacant, &portal),
            "состояние пересобрано: старое несёт пути пустого слота"
        );
        assert_eq!(reg.get("acme").map(|s| s.portal_id()).as_deref(), Some("acme"));
        assert!(portal.has_webhook().await);
        // Автомат подключения `add` НЕ трогает: его двигает проверка, которую
        // делает вызывающий. Выставь реестр здесь `disconnected` — и живой
        // экран подключения мигал бы «нет связи» между вставкой вебхука и
        // первым `checking`.
        assert_eq!(portal.login_state_dto().await.state, "not_configured");

        reg.shutdown_all().await;
    }

    #[tokio::test]
    async fn add_refuses_an_empty_webhook() {
        let dir = tempfile::tempdir().unwrap();
        let reg = reg_from_file(dir.path());
        assert!(reg.add(&WebhookUrl::default(), None).await.is_err());
        assert_eq!(reg.len(), 1);
        assert!(reg.connected().is_empty());
    }

    /// КРИТЕРИЙ Ф2.3: воркеры удалённого портала завершились, соседа — живы,
    /// секрет исчез из файла, чужие ключи и права `0600` целы.
    #[tokio::test]
    async fn remove_stops_only_its_own_portal_and_erases_only_its_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        // Чужой ключ пользователя, который обязан пережить удаление портала.
        std::fs::write(&path, "poll_active_secs = 7\n").unwrap();
        let acme = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/");
        let beta = WebhookUrl::new("https://beta.bitrix24.ru/rest/2/bbb/");
        config::save_portals_to(
            &path,
            &[
                PortalConfig::from_webhook(&acme, None),
                PortalConfig::from_webhook(&beta, None),
            ],
        )
        .unwrap();

        let reg = reg_from_file(dir.path());
        reg.spawn_all().await;
        let gone = reg.get("acme").unwrap();
        let kept = reg.get("beta").unwrap();
        assert_eq!(gone.workers.lock().await.len(), 5);

        assert!(reg.remove("acme", false).await.unwrap());

        assert_eq!(reg.ids(), vec!["beta"]);
        assert!(reg.get("acme").is_none());
        assert!(gone.shutdown.is_cancelled(), "воркеры удалённого разбужены");
        assert!(
            gone.workers.lock().await.is_empty(),
            "воркеры удалённого портала завершились"
        );
        assert!(!kept.shutdown.is_cancelled(), "сосед не тронут");
        assert_eq!(kept.workers.lock().await.len(), 5, "воркеры соседа живы");

        // Файл: секрет удалённого исчез, секрет соседа и чужой ключ целы.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("aaa"), "секрет удалённого портала стёрт");
        assert!(body.contains("bbb"), "секрет соседа на месте");
        assert!(body.contains("poll_active_secs"), "чужой ключ цел");
        assert_perms_0600(&path);

        // Идемпотентность: повторное удаление не паникует и не врёт.
        assert!(!reg.remove("acme", false).await.unwrap());
        assert!(!reg.remove("никого-нет", false).await.unwrap());

        reg.shutdown_all().await;
    }

    /// Удаление ПОСЛЕДНЕГО портала возвращает экран подключения — как logout.
    #[tokio::test]
    async fn removing_the_last_portal_restores_the_vacant_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let w = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/");
        config::save_portals_to(&path, &[PortalConfig::from_webhook(&w, None)]).unwrap();

        let reg = reg_from_file(dir.path());
        reg.spawn_all().await;
        assert!(reg.remove("acme", false).await.unwrap());

        assert_eq!(reg.len(), 1, "список никогда не пуст");
        assert!(reg.connected().is_empty());
        let slot = reg.primary();
        assert_eq!(slot.portal_id(), "");
        assert!(!slot.has_webhook().await);
        assert_eq!(slot.login_state_dto().await.state, "not_configured");
        assert_eq!(
            slot.workers.lock().await.len(),
            4,
            "экран подключения ожил без рестарта: у пустого слота свои воркеры"
        );

        // В файле не осталось ни массива порталов, ни legacy-ключа.
        let reread = BitrixRootConfig::load_from_file(&path);
        assert!(reread.portals.is_empty());

        reg.shutdown_all().await;
    }

    /// `purge_cache` сносит каталог ОТКЛЮЧАЕМОГО портала — и только его.
    ///
    /// Проверка внутри [`purge_portal_cache`] («каталог отличен от общего и
    /// лежит внутри него») написана была ещё до Ф6 и до неё оставалась
    /// бездействующей: все порталы делили один каталог, и сносить его значило
    /// бы стереть кэш соседей. С пер-портальными подкаталогами она заработала
    /// сама — и этот тест держит ОБЕ её половины: свой каталог исчезает, чужой
    /// и общий корень остаются.
    #[tokio::test]
    async fn purge_cache_removes_only_the_directory_of_the_removed_portal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        config::save_portals_to(
            &path,
            &[
                PortalConfig::from_webhook(
                    &WebhookUrl::new("https://acme.bitrix24.ru/rest/1/aaa/"),
                    None,
                ),
                PortalConfig::from_webhook(
                    &WebhookUrl::new("https://other.bitrix24.ru/rest/1/bbb/"),
                    None,
                ),
            ],
        )
        .unwrap();

        let reg = reg_from_file(dir.path());
        let shared_cache = reg.shared().cache_dir.clone();
        let mine = reg.get("acme").unwrap().config.cache_dir.clone();
        let neighbour = reg.get("other").unwrap().config.cache_dir.clone();
        assert_ne!(mine, neighbour, "у порталов РАЗНЫЕ каталоги");

        for d in [&shared_cache, &mine, &neighbour] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("payload.bin"), b"x").unwrap();
        }

        assert!(reg.remove("acme", true).await.unwrap());

        assert!(!mine.exists(), "каталог отключённого портала снесён");
        assert!(
            neighbour.join("payload.bin").is_file(),
            "кэш соседа переживает отключение чужой вкладки"
        );
        assert!(
            shared_cache.join("payload.bin").is_file(),
            "общий корень не трогается никогда"
        );

        reg.shutdown_all().await;
    }

    /// Права `0600` — инвариант записи конфига, а не разовая любезность.
    fn assert_perms_0600(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "права конфига: {mode:o}");
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    #[tokio::test]
    async fn rename_touches_the_title_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let webhook = WebhookUrl::new("https://acme.bitrix24.ru/rest/1/secret42/");
        config::save_portals_to(&path, &[PortalConfig::from_webhook(&webhook, None)]).unwrap();

        let root = BitrixRootConfig::load_from_file(&path);
        let mut root = root;
        root.shared.cache_dir = dir.path().join("cache");
        root.shared.prompts_path = dir.path().join("prompts.json");
        root.shared.claude_cli_path = dir.path().join("no-such-claude");
        let reg = B24Registry::new(root).unwrap();

        assert!(reg.rename("acme", "  Наш портал  ").await.unwrap());
        assert_eq!(reg.get("acme").unwrap().portal_title(), "Наш портал");

        // Секрет на месте и в памяти, и в файле.
        let reread = BitrixRootConfig::load_from_file(&path);
        assert_eq!(reread.portals.len(), 1);
        assert_eq!(reread.portals[0].id, "acme");
        assert_eq!(reread.portals[0].title, "Наш портал");
        assert_eq!(reread.portals[0].webhook.expose(), webhook.expose());

        // Неизвестный портал — не ошибка и не паника.
        assert!(!reg.rename("nope", "x").await.unwrap());
    }

    #[tokio::test]
    async fn reorder_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bitrix.toml");
        let portals: Vec<PortalConfig> = ["acme", "beta", "gamma"]
            .iter()
            .map(|h| {
                PortalConfig::from_webhook(
                    &WebhookUrl::new(format!("https://{h}.bitrix24.ru/rest/1/k/")),
                    None,
                )
            })
            .collect();
        config::save_portals_to(&path, &portals).unwrap();

        let mut root = BitrixRootConfig::load_from_file(&path);
        root.shared.cache_dir = dir.path().join("cache");
        root.shared.prompts_path = dir.path().join("prompts.json");
        root.shared.claude_cli_path = dir.path().join("no-such-claude");
        let reg = B24Registry::new(root).unwrap();
        assert_eq!(reg.ids(), vec!["acme", "beta", "gamma"]);

        // Неполный и грязный список: непомянутая gamma уезжает в хвост,
        // выдуманный zzz игнорируется.
        reg.reorder(&["beta".into(), "zzz".into(), "acme".into()])
            .await
            .unwrap();
        assert_eq!(reg.ids(), vec!["beta", "acme", "gamma"]);

        // Порядок доехал до диска — иначе он забывался бы при перезапуске.
        let reread = BitrixRootConfig::load_from_file(&path);
        assert_eq!(
            reread.portal_ids(),
            vec!["beta".to_string(), "acme".into(), "gamma".into()]
        );
    }
}
