//! Жизненный цикл дорожек: кто под каким номером пишется.
//!
//! Модуль НЕ импортирует `axum` и `http`.
//!
//! # ГЛАВНОЕ ПРАВИЛО: ИНДЕКС НЕ ПЕРЕИСПОЛЬЗУЕТСЯ
//!
//! Дорожки появляются и исчезают в течение встречи: участник вышел и вернулся,
//! SFU переложил его в другой слот, сеть моргнула и произошла renegotiation.
//! Если после `ended` отдать освободившийся номер новой дорожке, звук ДВУХ
//! РАЗНЫХ ЛЮДЕЙ попадёт в один `raw/track-N.wav` — и никакая последующая
//! атрибуция этого уже не разберёт: в файле просто два голоса подряд, и
//! признака, где кончился первый, в нём нет.
//!
//! Цена правила — несколько лишних файлов по несколько килобайт. Цена его
//! нарушения — испорченная запись, которую нельзя восстановить.
//!
//! # Renegotiation даёт СМЕЩЕНИЕ, а не новую ось
//!
//! `T0` остаётся тем же на всю встречу. Дорожка, появившаяся на пятнадцатой
//! минуте, получает `start_offset_ms = 900_000` — и это же значение потом
//! становится параметром `adelay` в ffmpeg, ставящим её на общую ось. Заводить
//! дорожке свой ноль означало бы вручную сводить таймлайны при монтаже.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Состояние одной дорожки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackState {
    /// Идентификатор дорожки платформы (`MediaStreamTrack.id`).
    pub track_id: String,
    /// Номер файла `raw/track-N.wav`. **Уникален на всю встречу.**
    pub idx: usize,
    /// Смещение начала дорожки на общей оси, мс.
    pub start_offset_ms: u64,
    /// Смещение конца. `None` — дорожка ещё жива.
    pub end_offset_ms: Option<u64>,
    /// Сколько кадров принято.
    pub frames: u64,
    /// Сколько сэмплов записано (без учёта дырок от потерянных кадров).
    pub samples: u64,
    /// Наибольшее сэмпловое смещение + длина кадра: фактическая длина файла.
    pub end_samples: u64,
    /// Каким тапом взята ИМЕННО ЭТА дорожка.
    ///
    /// Общий [`crate::ingest::Manifest::capture_path`] отвечает на вопрос
    /// «какой путь победил», а здесь — «откуда взялась вот эта дорожка».
    /// Разница важна ровно тогда, когда разбирают запись: дорожка, взятая
    /// зеркалом Web Audio при живом WebRTC, — это дубль, и увидеть его надо
    /// по манифесту, а не переслушивая звук.
    ///
    /// `None` у записей, сделанных до появления поля, и у дорожек, заведённых
    /// неявно по первому кадру.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_path: Option<String>,
}

impl TrackState {
    /// Имя файла дорожки относительно каталога встречи.
    ///
    /// Номер, а не идентификатор платформы: `MediaStreamTrack.id` — это
    /// длинный UUID, и каталог из таких имён нечитаем, а разбирать записи
    /// руками приходится regularly.
    pub fn file_name(&self) -> String {
        format!("track-{}.wav", self.idx)
    }

    /// Жива ли дорожка.
    pub fn is_live(&self) -> bool {
        self.end_offset_ms.is_none()
    }

    /// Длительность по фактически записанному звуку, мс.
    pub fn recorded_ms(&self, sample_rate: u32) -> u64 {
        if sample_rate == 0 {
            return 0;
        }
        self.end_samples * 1000 / u64::from(sample_rate)
    }
}

/// Реестр дорожек встречи.
#[derive(Debug, Default)]
pub struct TrackRegistry {
    /// Все дорожки в порядке появления. Закрытые остаются здесь: манифест
    /// обязан помнить их, иначе после встречи было бы непонятно, откуда взялся
    /// `track-3.wav` при трёх участниках.
    tracks: Vec<TrackState>,
    /// Идентификатор → позиция в [`TrackRegistry::tracks`], **только для
    /// живых**. Закрытая дорожка выбывает отсюда: пришедшая под тем же
    /// идентификатором заведёт НОВУЮ запись с новым номером.
    live: HashMap<String, usize>,
    next_idx: usize,
}

impl TrackRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Открывает дорожку и выдаёт ей НОВЫЙ номер.
    ///
    /// Повторное открытие живой дорожки — не ошибка: обход `getReceivers()`
    /// штатно натыкается на уже известную. Возвращается существующая запись.
    pub fn open(&mut self, track_id: &str, start_offset_ms: u64) -> &TrackState {
        if let Some(&pos) = self.live.get(track_id) {
            return &self.tracks[pos];
        }
        let idx = self.next_idx;
        self.next_idx += 1;
        self.tracks.push(TrackState {
            track_id: track_id.to_string(),
            idx,
            start_offset_ms,
            end_offset_ms: None,
            frames: 0,
            samples: 0,
            end_samples: 0,
            capture_path: None,
        });
        let pos = self.tracks.len() - 1;
        self.live.insert(track_id.to_string(), pos);
        &self.tracks[pos]
    }

    /// Запоминает тап, которым взята дорожка.
    ///
    /// Отдельным вызовом, а не параметром [`TrackRegistry::open`]: открыть
    /// дорожку может и первый пришедший кадр, у которого пути нет вовсе.
    pub fn note_path(&mut self, track_id: &str, path: &str) {
        if let Some(&pos) = self.live.get(track_id) {
            self.tracks[pos].capture_path = Some(path.to_string());
        }
    }

    /// Закрывает дорожку. Повторное закрытие игнорируется.
    ///
    /// Возвращает номер закрытой дорожки — вызывающему он нужен, чтобы
    /// дописать заголовок именно её файла.
    pub fn close(&mut self, track_id: &str, end_offset_ms: u64) -> Option<usize> {
        let pos = self.live.remove(track_id)?;
        self.tracks[pos].end_offset_ms = Some(end_offset_ms);
        Some(self.tracks[pos].idx)
    }

    /// Закрывает все живые дорожки — конец встречи.
    pub fn close_all(&mut self, end_offset_ms: u64) -> Vec<usize> {
        let ids: Vec<String> = self.live.keys().cloned().collect();
        let mut closed: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.close(id, end_offset_ms))
            .collect();
        closed.sort_unstable();
        closed
    }

    /// Учитывает принятый кадр. `None` — дорожка неизвестна.
    ///
    /// Кадр от УЖЕ ЗАКРЫТОЙ дорожки тоже учитывается (по её последней записи):
    /// worklet мог дофлашить хвост после события `ended`, и терять этот хвост
    /// незачем.
    pub fn note_frame(&mut self, track_id: &str, offset_samples: u64, samples: usize) -> Option<usize> {
        let pos = match self.live.get(track_id) {
            Some(&pos) => pos,
            // Дорожка закрыта — ищем её последнюю запись.
            None => self
                .tracks
                .iter()
                .rposition(|t| t.track_id == track_id)?,
        };
        let t = &mut self.tracks[pos];
        t.frames += 1;
        t.samples += samples as u64;
        t.end_samples = t.end_samples.max(offset_samples + samples as u64);
        Some(t.idx)
    }

    /// Дорожка по идентификатору (живая либо последняя закрытая).
    pub fn get(&self, track_id: &str) -> Option<&TrackState> {
        match self.live.get(track_id) {
            Some(&pos) => Some(&self.tracks[pos]),
            None => self.tracks.iter().rev().find(|t| t.track_id == track_id),
        }
    }

    /// Дорожка по номеру файла.
    pub fn by_idx(&self, idx: usize) -> Option<&TrackState> {
        self.tracks.iter().find(|t| t.idx == idx)
    }

    /// Все дорожки в порядке появления.
    pub fn all(&self) -> &[TrackState] {
        &self.tracks
    }

    /// Сколько дорожек живо сейчас. Вход [`crate::bot::detect_track_mode`].
    pub fn live_count(&self) -> usize {
        self.live.len()
    }

    /// Сколько дорожек было за встречу.
    pub fn total(&self) -> usize {
        self.tracks.len()
    }

    /// Следующий свободный номер. Существует ради теста инварианта.
    pub fn next_idx(&self) -> usize {
        self.next_idx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ИНВАРИАНТ ФАЗЫ: ушедший участник НЕ отдаёт свой номер пришедшему.
    #[test]
    fn a_finished_track_never_hands_its_index_over() {
        let mut r = TrackRegistry::new();
        assert_eq!(r.open("a", 0).idx, 0);
        assert_eq!(r.open("b", 100).idx, 1);

        r.close("a", 2_000);
        // Пришёл третий участник — номер 2, а не освободившийся 0.
        assert_eq!(r.open("c", 2_500).idx, 2, "номер переиспользован — звук двоих склеится");

        r.close("b", 3_000);
        r.close("c", 4_000);
        assert_eq!(r.open("d", 4_500).idx, 3);
        assert_eq!(r.total(), 4);

        // И номера уникальны все до одного.
        let mut seen = std::collections::HashSet::new();
        for t in r.all() {
            assert!(seen.insert(t.idx), "дубль номера {}", t.idx);
        }
    }

    /// Та же дорожка, вернувшаяся после `ended`, — это НОВАЯ дорожка.
    ///
    /// Идентификатор платформы после переподключения может совпасть, но между
    /// ними прошло время, и склеивать их в один файл нельзя: в записи
    /// образовалась бы дыра, которую невозможно отличить от молчания.
    #[test]
    fn the_same_id_coming_back_is_a_new_track() {
        let mut r = TrackRegistry::new();
        assert_eq!(r.open("a", 0).idx, 0);
        r.close("a", 1_000);
        let second = r.open("a", 5_000);
        assert_eq!(second.idx, 1, "вернувшаяся дорожка обязана получить новый номер");
        assert_eq!(second.start_offset_ms, 5_000);
        assert_eq!(r.total(), 2);
        // `get` отдаёт последнюю — она и пишется прямо сейчас.
        assert_eq!(r.get("a").unwrap().idx, 1);
    }

    /// Повторное открытие ЖИВОЙ дорожки — норма: обход `getReceivers()`
    /// штатно натыкается на уже известную.
    #[test]
    fn reopening_a_live_track_is_idempotent() {
        let mut r = TrackRegistry::new();
        assert_eq!(r.open("a", 0).idx, 0);
        assert_eq!(r.open("a", 999).idx, 0, "повторное открытие завело дубль");
        assert_eq!(r.get("a").unwrap().start_offset_ms, 0, "смещение не перезаписано");
        assert_eq!(r.total(), 1);
        assert_eq!(r.live_count(), 1);
    }

    /// Renegotiation: дорожка начинается ПОЗЖЕ нуля на общей оси, и это
    /// смещение потом становится `adelay` в ffmpeg.
    #[test]
    fn a_renegotiated_track_starts_with_an_offset_not_at_zero() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        let late = r.open("b", 900_000);
        assert_eq!(late.start_offset_ms, 900_000);
        assert_eq!(late.idx, 1);
        assert_eq!(late.file_name(), "track-1.wav");
    }

    #[test]
    fn frames_are_counted_and_the_length_follows_the_largest_offset() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        r.note_frame("a", 0, 320);
        r.note_frame("a", 320, 320);
        // Кадр НЕ ПО ПОРЯДКУ: длина определяется наибольшим смещением, а не
        // порядком прихода.
        r.note_frame("a", 960, 320);
        r.note_frame("a", 640, 320);

        let t = r.get("a").unwrap();
        assert_eq!(t.frames, 4);
        assert_eq!(t.samples, 4 * 320);
        assert_eq!(t.end_samples, 1_280, "длина обязана считаться по смещению");
        assert_eq!(t.recorded_ms(16_000), 80);
    }

    /// Хвост, дофлашенный после `ended`, не теряется.
    #[test]
    fn a_frame_arriving_after_the_track_ended_is_still_counted() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        r.note_frame("a", 0, 320);
        r.close("a", 20);
        assert_eq!(r.note_frame("a", 320, 320), Some(0), "хвост потерян");
        assert_eq!(r.get("a").unwrap().frames, 2);
    }

    #[test]
    fn closing_is_idempotent_and_unknown_tracks_are_reported() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        assert_eq!(r.close("a", 10), Some(0));
        assert_eq!(r.close("a", 20), None, "повторное закрытие не выдумывает дорожку");
        assert_eq!(r.close("нет такой", 10), None);
        assert_eq!(r.note_frame("нет такой", 0, 1), None);
        assert_eq!(r.get("нет такой"), None);
    }

    #[test]
    fn close_all_ends_the_meeting_for_every_live_track() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        r.open("b", 100);
        r.open("c", 200);
        r.close("b", 500);
        assert_eq!(r.live_count(), 2);

        assert_eq!(r.close_all(9_000), vec![0, 2]);
        assert_eq!(r.live_count(), 0);
        assert_eq!(r.get("a").unwrap().end_offset_ms, Some(9_000));
        assert_eq!(r.get("b").unwrap().end_offset_ms, Some(500), "уже закрытую не трогаем");
        assert!(r.all().iter().all(|t| !t.is_live()));
    }

    /// Состояние дорожек уезжает в манифест: без круговой сериализации
    /// прерванная запись не восстановилась бы.
    #[test]
    fn the_state_survives_a_json_round_trip() {
        let mut r = TrackRegistry::new();
        r.open("a", 0);
        r.note_frame("a", 0, 320);
        r.close("a", 20);
        r.open("b", 2_500);

        let json = serde_json::to_string(r.all()).unwrap();
        let back: Vec<TrackState> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r.all());
        assert_eq!(back[1].file_name(), "track-1.wav");
    }
}
