//! ТЕСТ-СТРАЖ Ф8: адреса скачивания портала не покидают процесс.
//!
//! # Что охраняется и почему цена ошибки максимальна
//!
//! Секрет плагина Bitrix24 — это ВЕСЬ URL вебхука целиком
//! (`https://<портал>/rest/<uid>/<код>/`), а не отдельный токен. Ответ
//! `im.v2.File.download` цитирует его дословно:
//!
//! ```json
//! {"downloadUrl":"https://portal.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/download/?token=…"}
//! ```
//!
//! Попади эта строка в браузер — и любой, кто увидел вкладку devtools, HAR-файл
//! поддержки или расширение браузера, получает ПОЛНЫЙ доступ к порталу: чтение
//! всей переписки, отправку от чужого имени, задачи. Это не утечка одного файла,
//! это компрометация учётки целиком.
//!
//! Рядом живут три адреса, которые портал присылает сам и которые НЕ РАБОТАЮТ
//! без сессионной куки (живая проверка: `urlDownload` и `urlShow` отдают HTML
//! страницы логина при HTTP 200, `unifiedLink` — 403). Их отсутствие в DTO —
//! отдельная ценность: попав во фронт, они дали бы «сломанные» ссылки, чинить
//! которые пришлось бы через поддержку.
//!
//! # Почему страж перебирает DTO, а не один роут
//!
//! Роут медиа сегодня один, а DTO переживают все двенадцать фаз. Регрессия
//! случится не в `routes/media.rs` (там правило написано в шапке), а в момент,
//! когда кто-нибудь добавит «полезное» поле в `MediaDto` или в `FileRaw`.
//! Поэтому страж сериализует ВСЁ, что плагин отдаёт наружу, и ищет запретные
//! подстроки в получившемся тексте.
//!
//! Второй рубеж — типы: [`forge_bitrix::api::schema::FileDownloadRaw`] не
//! реализует `Serialize`, поэтому положить адрес в ответ невозможно и по
//! ошибке. Третий — грep по исходникам ниже.

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use axum::Router;
use serde_json::json;
use tower::ServiceExt;

use forge_bitrix::attach::{
    ButtonDto, CardBlockDto, CardDto, CardGridRowDto, CardLinkDto, MediaDto, ReplyMarkupDto,
};
use forge_bitrix::config::{BitrixConfig, WebhookUrl};
use forge_bitrix::dto::{
    CountersDto, DialogDto, LastMessageDto, MessageDto, SenderDto,
};
use forge_bitrix::state::{B24State, B24UserDto, LoginFlow, LoginStateDto};

/// Портал и код вебхука, которых не должно быть НИГДЕ.
const PORTAL: &str = "test-work-2.bitrix24.ru";
const CODE: &str = "wqdzbfhmn83d23j4";

/// Подстроки, любая из которых в проводном тексте означает регрессию.
///
/// Первые четыре — имена полей портала: их присутствие означает, что кто-то
/// объявил поле «на всякий случай». Остальные — сам секрет и его форма.
const FORBIDDEN: [&str; 8] = [
    "downloadUrl",
    "download_url",
    "urlDownload",
    "urlShow",
    "unifiedLink",
    "unified_link",
    PORTAL,
    CODE,
];

/// Проверяет один сериализованный DTO.
fn assert_clean(label: &str, value: serde_json::Value) {
    let text = serde_json::to_string(&value).unwrap();
    for needle in FORBIDDEN {
        assert!(
            !text.contains(needle),
            "{label} содержит запретную подстроку {needle:?}: {text}"
        );
    }
}

/// ГЛАВНЫЙ ТЕСТ ФАЗЫ 8 (имя зафиксировано планом, §8/Ф8 — менять нельзя).
///
/// Собирает ВСЕ DTO, которые плагин отдаёт наружу, наполняет их значениями,
/// цитирующими адреса портала, сериализует и проверяет, что ни одна запретная
/// подстрока в текст не попала.
///
/// Тест обязан ПАДАТЬ, если добавить `downloadUrl` в [`MediaDto`]: именно этой
/// правки он и ждёт от будущих фаз.
#[test]
fn download_urls_never_leak_to_the_client() {
    // Значения, которыми злоумышленник (или невнимательный разработчик) мог бы
    // протащить адрес: имя файла, подпись кнопки, текст карточки — всё это
    // ЧУЖОЙ ТЕКСТ, и он не обязан быть безопасным. Страж проверяет форму DTO, а
    // не содержимое переписки, поэтому здесь их нет: подставь мы адрес в
    // `name`, тест ловил бы сообщение собеседника, а не дефект плагина.

    // 1. Вложение — главный подозреваемый.
    assert_clean(
        "MediaDto",
        serde_json::to_value(MediaDto {
            index: 0,
            file_id: 95,
            kind: "file".into(),
            name: Some("forge_probe.txt".into()),
            extension: Some("txt".into()),
            size: Some(500),
            width: Some(72),
            height: Some(61),
            has_thumb: true,
            has_full: true,
            player_url: None,
            waveform: vec![],
        })
        .unwrap(),
    );

    // Форма ключей проверяется отдельно: набор полей — это и есть контракт.
    // Сравнение по СОСТАВУ, а не по порядку: `serde_json::Value` держит объект
    // в отсортированной карте, и порядок объявления полей до текста не доезжает.
    let media = serde_json::to_value(MediaDto::default()).unwrap();
    let mut keys: Vec<&str> = media.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "index", "file_id", "kind", "name", "extension", "size", "width", "height",
        "has_thumb", "has_full", "player_url", "waveform",
    ];
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "состав полей MediaDto изменился — проверь, не появился ли адрес портала"
    );
    assert!(
        media["player_url"].is_null() && media["waveform"].as_array().unwrap().is_empty(),
        "player_url и waveform у Битрикс24 ВСЕГДА пусты: страницы-плеера и \
         огибающей голосового у платформы нет вовсе"
    );

    // 2. Сообщение целиком — вместе с вложением, карточками и клавиатурой.
    assert_clean(
        "MessageDto",
        serde_json::to_value(MessageDto {
            id: 113,
            dialog_id: "chat27".into(),
            out: false,
            date: Some("2026-08-05T20:20:36+03:00".into()),
            sender: Some(SenderDto {
                id: "1".into(),
                name: Some("Игорь".into()),
                color: Some("#df532d".into()),
                bot: false,
                avatar_url: Some("/api/bitrix/media/avatar/1".into()),
            }),
            system: false,
            text: "вложение".into(),
            segments: vec![],
            media: vec![MediaDto {
                index: 0,
                file_id: 95,
                kind: "image".into(),
                has_full: true,
                ..MediaDto::default()
            }],
            cards: vec![CardDto {
                id: Some("1".into()),
                color: Some("#29619b".into()),
                description: Some("карточка робота".into()),
                blocks: vec![CardBlockDto {
                    kind: "grid".into(),
                    rows: vec![CardGridRowDto {
                        name: Some("Файл".into()),
                        value: Some("forge_probe.txt".into()),
                        ..CardGridRowDto::default()
                    }],
                    links: vec![CardLinkDto {
                        name: Some("Открыть".into()),
                        url: Some("https://example.com/x".into()),
                        desc: None,
                    }],
                    ..CardBlockDto::default()
                }],
            }],
            reply_markup: Some(ReplyMarkupDto {
                kind: "inline".into(),
                rows: vec![vec![ButtonDto {
                    text: "Скачать".into(),
                    action: "put".into(),
                    enabled: true,
                    ..ButtonDto::default()
                }]],
            }),
            edited: false,
            likes: vec!["1".into()],
            deleted: false,
            action: None,
            unread: false,
            reply_to: None,
            fwd: None,
        })
        .unwrap(),
    );

    // 3. Строка списка — здесь живёт аватар (Р6).
    let dialog = DialogDto {
        id: "chat27".into(),
        chat_id: Some(27),
        kind: "chat".into(),
        title: "FORGE probe chat".into(),
        avatar_url: Some("/api/bitrix/media/avatar/1".into()),
        color: Some("#df532d".into()),
        unread_count: 2,
        last_message: Some(LastMessageDto {
            id: 113,
            text: "вложение".into(),
            date: Some("2026-08-05T20:20:36+03:00".into()),
            ..LastMessageDto::default()
        }),
        ..DialogDto::default()
    };
    assert_clean("DialogDto", serde_json::to_value(&dialog).unwrap());
    assert!(
        dialog.avatar_url.as_deref().unwrap().starts_with("/api/bitrix/media/avatar/"),
        "аватар обязан быть ПУТЁМ ПРОКСИ, а не адресом CDN портала (Р6)"
    );

    // 4. Состояние подключения и счётчики.
    assert_clean(
        "LoginStateDto",
        serde_json::to_value(LoginStateDto {
            state: "authorized".into(),
            user: Some(B24UserDto {
                id: "1".into(),
                name: Some("Игорь".into()),
                ..Default::default()
            }),
            ..LoginStateDto::default()
        })
        .unwrap(),
    );
    assert_clean(
        "CountersDto",
        serde_json::to_value(CountersDto {
            total: 9,
            chat: 0,
            dialog: 4,
            tasks: 0,
            notify: 5,
        })
        .unwrap(),
    );

    // 5. Сырой дескриптор файла портала: полей `urlDownload`/`urlShow`/
    //    `unifiedLink` в нём нет вовсе — не объявить проще, чем не отдать.
    let raw: forge_bitrix::api::schema::FileRaw = serde_json::from_value(json!({
        "id": 95, "chatId": 27, "name": "forge_probe.txt", "extension": "txt",
        "size": 500, "type": "file", "image": false, "status": "done", "authorId": 1,
        "urlPreview": format!("https://{PORTAL}/preview"),
        "urlDownload": format!("https://{PORTAL}/rest/1/{CODE}/download/"),
        "urlShow": format!("https://{PORTAL}/show"),
        "viewerAttrs": {"unifiedLink": format!("https://{PORTAL}/file/abc")},
        "mediaUrl": {"preview": {"250": ""}}
    }))
    .unwrap();
    // Проекция обязана донести до фронта всё, кроме адресов.
    let projected = forge_bitrix::attach::media_from_file(0, &raw);
    assert_eq!(projected.file_id, 95);
    assert_eq!(projected.name.as_deref(), Some("forge_probe.txt"));
    assert_clean(
        "media_from_file(живой дескриптор с адресами)",
        serde_json::to_value(projected).unwrap(),
    );
}

/// ВТОРОЙ РУБЕЖ: ответ роута медиа не редиректит на `downloadUrl`.
///
/// Редирект был бы вдвое дешевле по трафику — и означал бы отдать браузеру код
/// вебхука целиком. Поэтому заголовка `Location` не бывает ни в одном ответе
/// модуля: ни на успехе, ни на отказе, ни на враждебном ключе.
#[tokio::test]
async fn the_media_routes_never_answer_with_a_location_header() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BitrixConfig {
        webhook: WebhookUrl::new(format!("https://{PORTAL}/rest/1/{CODE}/")),
        // ЗАВЕДОМО ЗАКРЫТЫЙ порт: состояние здесь `authorized`, поэтому роут
        // дойдёт до сетевого вызова, и без подмены базы тест стучался бы в
        // ЖИВОЙ портал разведки. Он ответил бы отказом, но платой были бы
        // секунды таймаута на каждый прогон и запрос наружу из `cargo test`.
        base_url_override: "http://127.0.0.1:1/rest/1/closed".into(),
        cache_dir: dir.path().join("cache"),
        // Герметичность: без явного пути хранилище заметок легло бы в живой
        // ~/.forge/bitrix-chat-prompts.json разработчика, а CLI-заглушка
        // гарантирует, что тест не запустит настоящий claude.
        prompts_path: dir.path().join("prompts.json"),
        claude_cli_path: dir.path().join("no-such-claude"),
        config_path: dir.path().join("bitrix.toml"),
        ..BitrixConfig::default()
    };
    let state = B24State::new(cfg);
    state
        .set_login(LoginFlow::Authorized {
            user: B24UserDto {
                id: "1".into(),
                ..Default::default()
            },
            portal: PORTAL.into(),
        })
        .await;
    let app: Router = forge_bitrix::routes::build_router(forge_bitrix::B24Registry::single(state));

    for uri in [
        "/api/bitrix/media/chat27/113/0",
        "/api/bitrix/media/file/chat27/113/0",
        "/api/bitrix/media/avatar/1",
        "/api/bitrix/media/avatar/0",
        "/api/bitrix/media/%2e%2e/113/0",
    ] {
        let resp: Response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(
            !resp.status().is_redirection(),
            "{uri} ответил редиректом {}",
            resp.status()
        );
        assert!(
            resp.headers().get(axum::http::header::LOCATION).is_none(),
            "{uri} отдал заголовок Location"
        );
        // Тело отказа тоже проверяем: конверт ошибки цитирует причину, и туда
        // адрес попасть не должен.
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body).to_string();
        for needle in FORBIDDEN {
            assert!(
                !text.contains(needle),
                "{uri} ({status}) вернул тело с {needle:?}: {text}"
            );
        }
    }
}

/// ТРЕТИЙ РУБЕЖ: ни одна строка исходников не логирует адрес скачивания.
///
/// Построчный греп по `src/`: строка, в которой рядом стоят `download_url` (или
/// `downloadUrl`) и tracing-макрос, — это утечка секрета в лог, который потом
/// уезжает в файл, в journald и в отчёт об инциденте.
///
/// Тот же приём, что у стража `no_source_line_logs_the_exposed_url` (Ф2): грубо,
/// зато не требует запуска и ловит регрессию в момент её появления.
#[test]
fn no_source_line_logs_a_download_url() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    assert!(files.len() > 5, "исходники не найдены: {}", root.display());

    for path in files {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            // Комментарии объясняют этот самый запрет и цитируют имя поля
            // словами — искать в них бессмысленно.
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            let mentions_url = code.contains("download_url") || code.contains("downloadUrl");
            let logs = ["tracing::info!", "tracing::warn!", "tracing::debug!",
                        "tracing::trace!", "tracing::error!", "println!", "eprintln!"]
                .iter()
                .any(|m| code.contains(m));
            if mentions_url && logs {
                offenders.push(format!("{}:{}: {code}", path.display(), n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "адрес скачивания уходит в лог:\n{}",
        offenders.join("\n")
    );
}

/// Рекурсивно собирает `.rs`-файлы.
fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Компиляционный инвариант: `FileDownloadRaw` НЕ сериализуется.
///
/// Тест ничего не выполняет — он существует ради строки ниже. Реализуй кто-то
/// `Serialize` для этой структуры «чтобы удобнее логировать», и адрес стало бы
/// возможно положить в JSON одним вызовом; сейчас это не компилируется.
#[test]
fn the_download_response_type_is_not_serializable() {
    fn assert_not_serialize<T>() {}
    assert_not_serialize::<forge_bitrix::api::schema::FileDownloadRaw>();

    // Дословная проверка по исходнику: производные у структуры перечислены
    // явно, и `Serialize` среди них нет.
    let schema = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api/schema.rs"),
    )
    .unwrap();
    let at = schema
        .find("pub struct FileDownloadRaw")
        .expect("структура FileDownloadRaw исчезла");
    let derive_line = schema[..at]
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("#[derive"))
        .unwrap_or_default();
    assert!(
        !derive_line.contains("Serialize"),
        "у FileDownloadRaw появился Serialize: {derive_line}"
    );
}
