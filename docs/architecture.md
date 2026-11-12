# Архитектура

Факты о станции, на которых она стоит, — [max.md](max.md).

## Процессы

| Процесс | Что делает |
|---|---|
| `pulsar-max` (Rust, root, `aarch64-linux-android`) | ESPHome API и mDNS из крейтов pulsar, контроллер голоса и плеера, кольцо (`fb5`), кнопки (evdev) |
| аудио-помощник (Java, root, `app_process`) | `AudioRecord` 16 каналов 48 кГц с VQE `audioserver`, отдаёт PCM демону по pipe |

Захват только через Java `AudioRecord`: VQE Яндекса применяет `audioserver`, и 16 каналов через
`setChannelIndexMask` — путь, которым пишет сам `io.sdk`. AAudio в Android 9 ограничен 8
каналами.

Модель исполнения демона — как в pulsar
([architecture.md pulsar, «Модель исполнения»](https://github.com/Morvex885/pulsar/blob/v0.6.1/docs/architecture.md#модель-исполнения)):
один рантайм tokio `current_thread` и `LocalSet`, сервисы — акторы с неблокирующими handle,
`unsafe` и libc — только в `sys.rs`.

## Сосуществование со стеком Яндекса

`io.sdk` нельзя ни отключить (`pm disable` — protected package), ни остановить: его поднимают
привязки системных приложений. Поэтому он остаётся работать, а демон забирает у него микрофон:
`cmd appops set com.yandex.io.sdk RECORD_AUDIO ignore` и перезапуск `io.sdk`. Алиса перестаёт
слышать; часы на экране остаются за `io.sdk`. Крутилку громкости `io.sdk` тоже держит открытой;
останется ли она рабочей и видна ли громкость демону — не проверено.

## Автозапуск

init-сервис в `/odm/etc/init/` (как у root-метода), бинарь и данные — в `/data/pulsar-max/`.
Файлы root-метода не трогаются.

## Этапы

1. Каркас: сборка под Android, ESPHome API и mDNS на станции. Готово.
2. Голос: аудио-помощник, канал `vqe_0` 48 → 16 кГц, голосовой run в HA по кнопке.
3. Воспроизведение TTS и медиа, громкость.
4. Ключевое слово на станции: openWakeWord (по образцу pulsar PR #77), затем модель «Алиса».
5. Кольцо: состояния голоса; экран — по возможности.
6. Автозапуск, установка и откат.
7. Sendspin (Music Assistant), Zigbee-мост к `/dev/lp_uart`.
