//! Статус-машина встречи: чистая таблица переходов.
//!
//! Модуль НЕ импортирует `axum` и `http` и не делает ввода-вывода — здесь
//! только значения и функции над ними. Изоляция та же, что даёт листовой
//! крейт, но без второго крейта с одним потребителем.
//!
//! ```text
//! queued → launching → joining → [waiting_admit] → recording → leaving →
//!   transcoding → transcribing → [summarizing] → ready
//! плюс failed(reason) и cancelled
//! ```
//!
//! # Три правила, ради которых модуль существует
//!
//! **1. Статуса `detected` в базе НЕТ.** Строка встречи появляется только по
//! клику пользователя. Иначе таблица росла бы от простого скролла ленты: у
//! каждого сообщения со ссылкой была бы своя запись, и через месяц переписки
//! их набрались бы тысячи — при нулевом числе записанных встреч. Отсутствие
//! бейджа в ответе `GET /api/meet/badges` и означает «можно записать».
//!
//! **2. `stop` и `cancel` — РАЗНЫЕ операции.** `stop` значит «встреча
//! кончилась, обработай»: файлы сохраняются, пайплайн продолжается.
//! `cancel` значит «прерви и выброси». Из `Recording` они ведут в разные
//! места — [`Leaving`](MeetStatus::Leaving) и
//! [`Cancelled`](MeetStatus::Cancelled) — и это единственная причина, по
//! которой их нельзя было сделать одной кнопкой.
//!
//! **3. `failed` — только когда НЕТ ТРАНСКРИПТА.** Поэтому из
//! [`Summarizing`](MeetStatus::Summarizing) переход в
//! [`Failed`](MeetStatus::Failed) ЗАПРЕЩЁН таблицей: провал Claude переводит
//! `summarizing → ready` с [`SummaryStatus::Failed`]. Час расшифровки не имеет
//! права пропасть из-за неответившего саммаризатора.
//!
//! # Терминальные статусы и повтор этапа (Ф6)
//!
//! `ready` / `failed` / `cancelled` терминальны в том смысле, что САМИ по себе
//! никуда не двигаются. Единственный выход из них — явное действие человека
//! `POST /api/meet/meetings/:id/retry?from=`, и оно объявлено ЗДЕСЬ, строками
//! таблицы, а не мутацией статуса в обход: exhaustive-тест матрицы ломается на
//! первой же необъявленной паре, и это единственный способ не получить второй,
//! необнаруживаемый путь смены статуса.
//!
//! Что откуда можно повторить, определяется одним вопросом — **что осталось на
//! диске**:
//!
//! | из | можно | почему |
//! |---|---|---|
//! | `ready` | `transcoding`, `transcribing`, `summarizing` | есть и звук, и транскрипт |
//! | `failed` | `transcoding`, `transcribing` | звук есть, транскрипта нет — иначе статус не был бы `failed` |
//! | `cancelled` | ничего | «прерви и выброси» означает, что данных не осталось |
//!
//! `failed → summarizing` запрещён не из осторожности: `failed` по правилу 3
//! достижим только там, где транскрипта ещё нет, а саммари без транскрипта —
//! это запрос к модели о пустом файле.

use serde::{Deserialize, Serialize};

/// Статус записи встречи.
///
/// Порядок вариантов совпадает с порядком жизненного цикла — на него
/// опирается таблица `EXPECTED` в тестах, читать её иначе было бы невозможно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetStatus {
    /// Пользователь нажал кнопку, слот записи ещё не занят.
    Queued,
    /// Запускаем Chrome.
    Launching,
    /// Chrome открыт, идёт вход во встречу (интерстишалы, pre-join, имя).
    Joining,
    /// Ждём, пока хост впустит из лобби (Teams, ограниченный доступ Meet).
    WaitingAdmit,
    /// Пишем звук.
    Recording,
    /// Выходим из встречи и дофлашиваем WAV.
    Leaving,
    /// ffmpeg: пер-трековые WAV на общей оси плюс архивный микс.
    Transcoding,
    /// whisper.cpp.
    Transcribing,
    /// Claude CLI считает саммари.
    Summarizing,
    /// Готово. Саммари могло и не получиться — у него СВОЙ статус
    /// ([`SummaryStatus`]).
    Ready,
    /// Транскрипта нет и не будет.
    Failed,
    /// Прервано пользователем, данные выброшены.
    Cancelled,
    /// **Файлы вычищены ретенцией.** Строка в базе осталась, каталога на диске
    /// больше нет.
    ///
    /// Отдельный статус, а не удаление строки, и не флаг. Удали мы строку —
    /// бейдж на старом сообщении превратился бы в «можно записать», то есть
    /// предложил бы войти ботом в давно прошедшую встречу. Флаг рядом со
    /// статусом дал бы то же самое другой ценой: два независимых поля, которые
    /// фронт обязан читать вместе, и первый же забывший про второе экран снова
    /// покажет «ГОТОВО» у записи, которой нет.
    Pruned,
}

impl MeetStatus {
    /// Все статусы. Единственный источник истины о их числе: массив
    /// фиксированной длины, поэтому добавление варианта ломает и
    /// [`MeetStatus::ALL`], и таблицу в тестах — а не проходит молча.
    pub const ALL: [MeetStatus; 13] = [
        MeetStatus::Queued,
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::WaitingAdmit,
        MeetStatus::Recording,
        MeetStatus::Leaving,
        MeetStatus::Transcoding,
        MeetStatus::Transcribing,
        MeetStatus::Summarizing,
        MeetStatus::Ready,
        MeetStatus::Failed,
        MeetStatus::Cancelled,
        MeetStatus::Pruned,
    ];

    /// Статусы, при которых встреча считается АКТИВНОЙ.
    ///
    /// Дословно тот же список, что в частичном уникальном индексе
    /// `ux_meetings_active_url` (см. `migrations/V001_init.sql`): именно он
    /// делает «одна активная запись на комнату» структурным свойством базы, а
    /// не мьютексом в памяти. Расхождение этих двух списков означало бы, что
    /// бот входит в одну встречу дважды, поэтому на совпадение есть тест.
    pub const ACTIVE: [MeetStatus; 6] = [
        MeetStatus::Queued,
        MeetStatus::Launching,
        MeetStatus::Joining,
        MeetStatus::WaitingAdmit,
        MeetStatus::Recording,
        MeetStatus::Leaving,
    ];

    /// Значение в колонке `meetings.status`. Оно же уезжает в WS-кадры и в
    /// JSON роутов, поэтому менять его — ломать и базу, и фронт разом.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Launching => "launching",
            Self::Joining => "joining",
            Self::WaitingAdmit => "waiting_admit",
            Self::Recording => "recording",
            Self::Leaving => "leaving",
            Self::Transcoding => "transcoding",
            Self::Transcribing => "transcribing",
            Self::Summarizing => "summarizing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Pruned => "pruned",
        }
    }

    /// Обратный разбор. `None` — строка не из этого перечисления (например,
    /// база записана более новой версией devforge).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// Конечное ли это состояние: дальше по своей воле встреча не двигается.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Failed | Self::Cancelled | Self::Pruned
        )
    }

    /// Остались ли на диске файлы встречи.
    ///
    /// `false` только у [`Pruned`](MeetStatus::Pruned): повтор этапа оттуда
    /// невозможен не по правилу, а физически — читать нечего.
    pub fn has_files(self) -> bool {
        self != Self::Pruned
    }

    /// Занимает ли встреча слот записи и строку в частичном уникальном индексе.
    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }

    /// Идёт ли уже пост-обработка (браузер закрыт, работает пайплайн).
    pub fn is_processing(self) -> bool {
        matches!(self, Self::Transcoding | Self::Transcribing | Self::Summarizing)
    }

    /// Есть ли на этой стадии транскрипт. Определяет главное правило модуля:
    /// [`Failed`](MeetStatus::Failed) разрешён только там, где транскрипта
    /// ещё нет.
    pub fn has_transcript(self) -> bool {
        matches!(self, Self::Summarizing | Self::Ready)
    }
}

/// Статус саммари — СВОЙ, отдельный от статуса встречи.
///
/// Ровно поэтому провал Claude не может превратить час расшифровки в
/// `failed`: у саммари есть куда записать свою неудачу.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SummaryStatus {
    /// Саммари не запрашивалось (выключено конфигом или ещё не дошли).
    #[default]
    None,
    /// В очереди либо считается.
    Pending,
    /// Готово.
    Ready,
    /// Claude не ответил или упал. **Встреча при этом `ready`.**
    Failed,
}

impl SummaryStatus {
    pub const ALL: [SummaryStatus; 4] = [
        SummaryStatus::None,
        SummaryStatus::Pending,
        SummaryStatus::Ready,
        SummaryStatus::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

/// Команда пользователя над идущей встречей.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetCommand {
    /// «Встреча кончилась, обработай». Файлы сохраняются.
    Stop,
    /// «Прерви и выброси».
    Cancel,
}

/// Разрешён ли переход `from → to`.
///
/// Единственная таблица переходов в крейте. Всё, что меняет
/// `meetings.status`, обязано спросить сначала здесь — иначе появился бы
/// второй, необнаруживаемый путь смены статуса.
pub fn can_transition(from: MeetStatus, to: MeetStatus) -> bool {
    use MeetStatus::*;

    // ВЫЧИСТКА ФАЙЛОВ ретенцией. Проверяется ПЕРВОЙ, раньше ветки `ready`:
    // иначе `ready → pruned` не дошёл бы сюда вовсе и подчистка молча не
    // смогла бы отметить то, что уже удалила с диска.
    //
    // Только из состояний, в которых встреча уже никуда не идёт: у активной
    // записи файлы пишутся прямо сейчас, а у пост-обработки они на входе.
    if to == Pruned {
        return matches!(from, Ready | Failed | Cancelled);
    }

    // ПОВТОР ЭТАПА — единственный выход из терминального состояния, и он
    // всегда начинается с явного действия человека. Что именно разрешено,
    // решает не «терминальность», а НАЛИЧИЕ ДАННЫХ: у `ready` есть и звук, и
    // транскрипт, у `failed` — обычно только звук.
    //
    // `cancelled` раньше выхода не имел вовсе — по предположению «у отменённой
    // встречи не осталось ничего». Живой кейс 07.09.2026 (Телемост,
    // f61cfa7a…) его опроверг: запись остановили кнопкой (`user_stop`),
    // пост-обработка отработала до конца — на диске звук, транскрипт и
    // саммари, — а статус после этого стал `cancelled`. Пересчитать саммари по
    // готовому транскрипту было нельзя: гейт отвечал `invalid_state`, хотя
    // данные лежали рядом.
    //
    // Поэтому терминальные состояния перечисляют один и тот же набор этапов, а
    // фактическое наличие файлов проверяет вызывающий (`routes::meetings::
    // start_stage`): статус — ярлык, а данные — на диске, и решать должен
    // диск.
    if from == Ready {
        return matches!(to, Transcoding | Transcribing | Summarizing);
    }
    // У `failed` транскрипта нет ПО ПОСТРОЕНИЮ: провал саммари даёт `ready`
    // плюс `SummaryStatus::Failed`, а до `failed` доходят только отказы на
    // записи и расшифровке. Поэтому саммари отсюда по-прежнему закрыто.
    if from == Failed {
        return matches!(to, Transcoding | Transcribing);
    }
    // `cancelled` может нести ЧТО УГОДНО: отмену жмут и на пустой комнате, и
    // после готовой пост-обработки. Поэтому набор этапов здесь такой же, как у
    // `ready`, а есть ли нужный файл — решает `routes::meetings::start_stage`
    // по диску.
    if from == Cancelled {
        return matches!(to, Transcoding | Transcribing | Summarizing);
    }

    // Из остальных терминальных состояний выхода нет.
    if from.is_terminal() {
        return false;
    }

    // Отмена доступна из любого нетерминального состояния: «прерви и выброси»
    // осмысленно и для зависшей расшифровки, а не только для записи.
    if to == Cancelled {
        return true;
    }

    // ГЛАВНОЕ ПРАВИЛО: `failed` только там, где транскрипта ещё нет.
    // Из `summarizing` переход запрещён — провал саммари даёт `ready` плюс
    // `SummaryStatus::Failed`.
    if to == Failed {
        return !from.has_transcript();
    }

    match (from, to) {
        (Queued, Launching) => true,
        (Launching, Joining) => true,
        // Лобби есть не у каждой платформы: Телемост пускает сразу, Teams —
        // через ожидание. Обе ветки законны.
        (Joining, WaitingAdmit | Recording) => true,
        (WaitingAdmit, Recording) => true,
        // Штатный выход — через `leaving`. Прямой `recording → transcoding`
        // существует ради `reconcile_on_boot`: после Ctrl-C браузера уже нет,
        // выходить неоткуда, а сорок минут звука на диске есть.
        (Recording, Leaving | Transcoding) => true,
        (Leaving, Transcoding) => true,
        (Transcoding, Transcribing) => true,
        // Саммари необязательно: выключено конфигом — сразу `ready`.
        (Transcribing, Summarizing | Ready) => true,
        (Summarizing, Ready) => true,
        _ => false,
    }
}

/// Куда ведёт команда пользователя из текущего статуса.
///
/// `None` означает «команда сейчас бессмысленна» — вызывающий отвечает
/// `409 invalid_state`, а не молча ничего не делает.
pub fn on_command(from: MeetStatus, cmd: MeetCommand) -> Option<MeetStatus> {
    use MeetStatus::*;

    match cmd {
        // «Прерви и выброси» — из любого нетерминального состояния.
        MeetCommand::Cancel => (!from.is_terminal()).then_some(Cancelled),
        MeetCommand::Stop => match from {
            // Записи ещё нет: обрабатывать нечего, поэтому `stop` здесь
            // совпадает с `cancel` по исходу. Это не дублирование команд —
            // из `Recording` они расходятся, и ровно там разница видна.
            Queued | Launching | Joining | WaitingAdmit => Some(Cancelled),
            // Единственная пара, ради которой команды разделены.
            Recording => Some(Leaving),
            // Уже выходим либо уже обрабатываем — повторный `stop` не
            // означает ничего нового.
            Leaving | Transcoding | Transcribing | Summarizing => None,
            Ready | Failed | Cancelled | Pruned => None,
        },
    }
}

/// Исход саммаризации: куда идёт встреча и что записывается в
/// `meetings.summary_status`.
///
/// Отдельная функция, а не `if` на месте вызова: правило «провал саммари не
/// роняет встречу» слишком легко нарушить копипастой из соседнего этапа, где
/// провал как раз означает `failed`.
pub fn on_summary_finished(ok: bool) -> (MeetStatus, SummaryStatus) {
    if ok {
        (MeetStatus::Ready, SummaryStatus::Ready)
    } else {
        (MeetStatus::Ready, SummaryStatus::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::MeetStatus::*;
    use super::*;

    /// ПОЛНАЯ матрица переходов, выписанная руками.
    ///
    /// Размер массива выведен из `MeetStatus::ALL.len()`, поэтому добавление
    /// статуса НЕ КОМПИЛИРУЕТСЯ, пока автор не допишет строку и столбец. Это и
    /// есть требование «новый статус без строки в тесте валит тест» —
    /// исполненное на этапе сборки, а не в рантайме.
    ///
    /// Строка — `from`, столбец — `to`, порядок обоих совпадает с
    /// [`MeetStatus::ALL`].
    #[rustfmt::skip]
    const EXPECTED: [[bool; MeetStatus::ALL.len()]; MeetStatus::ALL.len()] = [
        //          que    lau    joi    adm    rec    lea    tcod   tscr   summ   ready  fail   canc   prun
        /* que  */ [false, true,  false, false, false, false, false, false, false, false, true,  true,  false],
        /* lau  */ [false, false, true,  false, false, false, false, false, false, false, true,  true,  false],
        /* joi  */ [false, false, false, true,  true,  false, false, false, false, false, true,  true,  false],
        /* adm  */ [false, false, false, false, true,  false, false, false, false, false, true,  true,  false],
        /* rec  */ [false, false, false, false, false, true,  true,  false, false, false, true,  true,  false],
        /* lea  */ [false, false, false, false, false, false, true,  false, false, false, true,  true,  false],
        /* tcod */ [false, false, false, false, false, false, false, true,  false, false, true,  true,  false],
        /* tscr */ [false, false, false, false, false, false, false, false, true,  true,  true,  true,  false],
        /* summ */ [false, false, false, false, false, false, false, false, false, true,  false, true,  false],
        // Единицы в строках `ready`, `failed` и `cancelled` — это ПОВТОР
        // ЭТАПА (`retry?from=`), единственный выход из терминального
        // состояния.
        //
        // `failed` отличается от двух других ровно одним нулём — `summarizing`:
        // транскрипта у него нет ПО ПОСТРОЕНИЮ, потому что провал саммари даёт
        // `ready` плюс `SummaryStatus::Failed`, а до `failed` доходят только
        // отказы на записи и расшифровке.
        //
        // `cancelled` раньше был пустой строкой — по предположению «отменили,
        // значит данных не осталось». Живой кейс 07.09.2026 (Телемост,
        // f61cfa7a…) его опроверг: запись остановили кнопкой, пост-обработка
        // дошла до конца, а статус после этого стал `cancelled`; на диске
        // лежало всё, но пересчёт саммари отвечал `invalid_state`. Отмену жмут
        // и на пустой комнате, и после готовой обработки — ярлык статуса о
        // содержимом диска не знает, поэтому здесь набор такой же, как у
        // `ready`, а наличие конкретного файла проверяет
        // `routes::meetings::missing_input`.
        //
        // Последний столбец — ВЫЧИСТКА ФАЙЛОВ ретенцией. Она возможна ровно из
        // трёх законченных состояний и ни из одного идущего: у активной записи
        // файлы пишутся прямо сейчас, а у пост-обработки они на входе.
        /* ready*/ [false, false, false, false, false, false, true,  true,  true,  false, false, false, true ],
        /* fail */ [false, false, false, false, false, false, true,  true,  false, false, false, false, true ],
        /* canc */ [false, false, false, false, false, false, true,  true,  true,  false, false, false, true ],
        // Из `pruned` не ведёт НИЧЕГО: повтор этапа невозможен не по правилу,
        // а физически — на диске ничего не осталось.
        /* prun */ [false, false, false, false, false, false, false, false, false, false, false, false, false],
    ];

    #[test]
    fn transition_matrix_is_exhaustive() {
        for (i, from) in MeetStatus::ALL.into_iter().enumerate() {
            for (j, to) in MeetStatus::ALL.into_iter().enumerate() {
                assert_eq!(
                    can_transition(from, to),
                    EXPECTED[i][j],
                    "переход {} → {} объявлен в таблице как {}, а функция говорит другое",
                    from.as_str(),
                    to.as_str(),
                    EXPECTED[i][j]
                );
            }
        }
    }

    /// Правило 3 плана, вынесенное отдельным тестом: он должен падать с
    /// внятным текстом, а не теряться среди 144 ячеек матрицы.
    #[test]
    fn summarizing_can_never_fail_the_meeting() {
        assert!(
            !can_transition(Summarizing, Failed),
            "провал саммари не имеет права выбросить час расшифровки"
        );
        assert_eq!(on_summary_finished(false), (Ready, SummaryStatus::Failed));
        assert_eq!(on_summary_finished(true), (Ready, SummaryStatus::Ready));
    }

    /// `failed` разрешён ровно там, где транскрипта ещё нет.
    #[test]
    fn failed_is_allowed_exactly_where_there_is_no_transcript() {
        for from in MeetStatus::ALL {
            let expected = !from.is_terminal() && !from.has_transcript();
            assert_eq!(
                can_transition(from, Failed),
                expected,
                "{} → failed",
                from.as_str()
            );
        }
    }

    #[test]
    fn stop_and_cancel_diverge_from_recording() {
        assert_eq!(on_command(Recording, MeetCommand::Stop), Some(Leaving));
        assert_eq!(on_command(Recording, MeetCommand::Cancel), Some(Cancelled));
        assert_ne!(
            on_command(Recording, MeetCommand::Stop),
            on_command(Recording, MeetCommand::Cancel),
            "stop и cancel обязаны вести в разные места — иначе они одна кнопка"
        );
    }

    #[test]
    fn stop_before_any_audio_is_the_same_as_cancel() {
        for from in [Queued, Launching, Joining, WaitingAdmit] {
            assert_eq!(
                on_command(from, MeetCommand::Stop),
                Some(Cancelled),
                "{}: обрабатывать нечего",
                from.as_str()
            );
        }
    }

    #[test]
    fn stop_during_processing_is_a_noop_and_cancel_is_not() {
        for from in [Leaving, Transcoding, Transcribing, Summarizing] {
            assert_eq!(on_command(from, MeetCommand::Stop), None, "{}", from.as_str());
            assert_eq!(
                on_command(from, MeetCommand::Cancel),
                Some(Cancelled),
                "{}",
                from.as_str()
            );
        }
    }

    /// Терминальные статусы не принимают КОМАНД (`stop`/`cancel`) и не
    /// двигаются сами. Единственное, что из них возможно, — повтор этапа, и он
    /// не команда, а отдельное действие с явным указанием этапа.
    #[test]
    fn terminal_statuses_accept_no_command_and_move_only_on_retry() {
        for from in [Ready, Failed, Cancelled, Pruned] {
            assert!(from.is_terminal());
            assert_eq!(on_command(from, MeetCommand::Stop), None);
            assert_eq!(on_command(from, MeetCommand::Cancel), None);
        }
        // Из `cancelled` возможны ровно вычистка и повтор этапа.
        //
        // Раньше здесь стоял запрет на ВСЁ, кроме `pruned`, по предположению
        // «отменили — значит данных не осталось». Живой кейс 07.09.2026
        // (Телемост, f61cfa7a…) его опроверг: запись остановили кнопкой,
        // пост-обработка отработала до конца, а статус после этого стал
        // `cancelled` — на диске лежали и звук, и транскрипт, и саммари, но
        // пересчитать саммари было нельзя. Что именно доступно, решает диск:
        // см. `routes::meetings::missing_input`.
        let allowed_from_cancelled = [Pruned, Transcoding, Transcribing, Summarizing];
        for to in MeetStatus::ALL {
            assert_eq!(
                can_transition(Cancelled, to),
                allowed_from_cancelled.contains(&to),
                "cancelled → {}",
                to.as_str()
            );
        }
    }

    /// Живой кейс 07.09.2026 (Телемост, f61cfa7a…): запись остановили кнопкой
    /// (`user_stop`), пост-обработка дошла до конца — на диске звук, транскрипт
    /// и саммари, — а статус после этого стал `cancelled`. Пересчёт саммари
    /// отвечал `invalid_state`, хотя данные лежали рядом.
    ///
    /// Отмену жмут и на пустой комнате, и после готовой обработки, поэтому
    /// набор этапов у `cancelled` тот же, что у `ready`. Что доступно на самом
    /// деле, решает диск: `routes::meetings::missing_input`.
    #[test]
    fn a_cancelled_meeting_can_still_be_resummarised() {
        assert!(can_transition(Cancelled, Summarizing));
        assert!(can_transition(Cancelled, Transcribing));
        assert!(can_transition(Cancelled, Transcoding));

        // А вот у `failed` транскрипта нет по построению — саммари закрыто.
        assert!(!can_transition(Failed, Summarizing));

        // Из `pruned` по-прежнему не ведёт ничего: файлов физически нет.
        for to in MeetStatus::ALL {
            assert!(!can_transition(Pruned, to), "pruned → {}", to.as_str());
        }
    }

    /// ВЫЧИСТКА ФАЙЛОВ: только у законченной встречи, и только в одну сторону.
    ///
    /// Тест обязан падать внятно, а не теряться среди ячеек матрицы: ошибка
    /// здесь означает либо удалённый каталог идущей записи, либо повтор этапа
    /// поверх файлов, которых нет.
    #[test]
    fn pruning_is_possible_only_from_a_finished_meeting_and_leads_nowhere() {
        for from in [Ready, Failed, Cancelled] {
            assert!(can_transition(from, Pruned), "{} → pruned", from.as_str());
        }
        for from in MeetStatus::ALL {
            if matches!(from, Ready | Failed | Cancelled) {
                continue;
            }
            assert!(
                !can_transition(from, Pruned),
                "{} → pruned: у идущей записи файлы пишутся прямо сейчас",
                from.as_str()
            );
        }
        for to in MeetStatus::ALL {
            assert!(
                !can_transition(Pruned, to),
                "pruned → {}: на диске ничего не осталось",
                to.as_str()
            );
        }
        assert!(Pruned.is_terminal());
        assert!(!Pruned.has_files());
        for st in MeetStatus::ALL {
            assert_eq!(st.has_files(), st != Pruned, "{}", st.as_str());
        }
    }

    /// ПОВТОР ЭТАПА: что разрешено, определяется наличием данных на диске, а не
    /// вкусом. Тест обязан падать внятно, а не теряться среди 144 ячеек.
    #[test]
    fn retry_is_allowed_exactly_where_the_data_still_exists() {
        // Из `ready` есть и звук, и транскрипт.
        for to in [Transcoding, Transcribing, Summarizing] {
            assert!(can_transition(Ready, to), "ready → {}", to.as_str());
        }
        // Из `failed` есть только звук: транскрипта там нет по правилу 3.
        assert!(can_transition(Failed, Transcoding));
        assert!(can_transition(Failed, Transcribing));
        assert!(
            !can_transition(Failed, Summarizing),
            "саммари без транскрипта — это запрос к модели о пустом файле"
        );
        // Назад к записи повтор не ведёт: браузера давно нет, встреча кончилась.
        for to in [Queued, Launching, Joining, WaitingAdmit, Recording, Leaving] {
            assert!(!can_transition(Ready, to), "ready → {}", to.as_str());
            assert!(!can_transition(Failed, to), "failed → {}", to.as_str());
        }
    }

    /// Каждая команда обязана вести в РАЗРЕШЁННЫЙ переход: иначе роут получил
    /// бы целевой статус, который база отказалась бы принять.
    #[test]
    fn every_command_outcome_is_an_allowed_transition() {
        for from in MeetStatus::ALL {
            for cmd in [MeetCommand::Stop, MeetCommand::Cancel] {
                if let Some(to) = on_command(from, cmd) {
                    assert!(
                        can_transition(from, to),
                        "{:?} из {} ведёт в {}, но такой переход запрещён",
                        cmd,
                        from.as_str(),
                        to.as_str()
                    );
                }
            }
        }
    }

    /// У статуса тоже обязана быть ОДНА строковая форма.
    ///
    /// В колонку `meetings.status` пишется `as_str`, а в `MeetingBadge` и в
    /// WS-кадры уезжает serde. Разойдись они — фронт фильтровал бы ленту по
    /// строке, которой в базе нет.
    #[test]
    fn the_serde_form_equals_the_string_form() {
        for st in MeetStatus::ALL {
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{}\"", st.as_str()), "{st:?}");
            assert_eq!(serde_json::from_str::<MeetStatus>(&json).unwrap(), st);
        }
        for st in SummaryStatus::ALL {
            let json = serde_json::to_string(&st).unwrap();
            assert_eq!(json, format!("\"{}\"", st.as_str()), "{st:?}");
            assert_eq!(serde_json::from_str::<SummaryStatus>(&json).unwrap(), st);
        }
    }

    #[test]
    fn status_strings_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for st in MeetStatus::ALL {
            assert!(seen.insert(st.as_str()), "дубль строки {}", st.as_str());
            assert_eq!(MeetStatus::parse(st.as_str()), Some(st));
        }
        assert_eq!(MeetStatus::parse("detected"), None, "detected в базе не хранится");
        assert_eq!(MeetStatus::parse(""), None);

        let mut seen = std::collections::HashSet::new();
        for st in SummaryStatus::ALL {
            assert!(seen.insert(st.as_str()));
            assert_eq!(SummaryStatus::parse(st.as_str()), Some(st));
        }
    }

    /// Активные статусы — ровно те шесть, что стоят в частичном уникальном
    /// индексе. Расхождение означало бы, что бот входит в одну встречу дважды.
    #[test]
    fn active_statuses_match_the_partial_unique_index() {
        let sql = include_str!("../migrations/V001_init.sql");
        let start = sql
            .find("ux_meetings_active_url")
            .expect("индекс ux_meetings_active_url обязан быть в миграции");
        let tail = &sql[start..];
        let clause = &tail[..tail.find(';').expect("индекс не закрыт точкой с запятой")];
        for st in MeetStatus::ACTIVE {
            assert!(
                clause.contains(&format!("'{}'", st.as_str())),
                "статус {} активен в коде, но отсутствует в индексе",
                st.as_str()
            );
        }
        for st in MeetStatus::ALL {
            if st.is_active() {
                continue;
            }
            assert!(
                !clause.contains(&format!("'{}'", st.as_str())),
                "статус {} есть в индексе, но не считается активным в коде",
                st.as_str()
            );
        }
    }

    #[test]
    fn only_active_statuses_hold_a_recording_slot() {
        for st in MeetStatus::ALL {
            assert_eq!(st.is_active(), MeetStatus::ACTIVE.contains(&st));
            assert!(!(st.is_active() && st.is_terminal()));
            assert!(!(st.is_active() && st.is_processing()));
        }
    }
}
