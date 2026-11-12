# Станция Макс (`yandexstation_2`)

Замеры на станции владельца: ревизия с Zigbee (YNDX-00052), прошивка `5000000921`, `io.sdk`
3.367.25. Железо — [вики](https://hackalice.me/devices/station-max/) и
[ревизия с Zigbee](https://hackalice.me/devices/station-max-zigbee/); здесь только то, чего там нет.

## Платформа

- Android 9 (YaOS_X), не Buildroot: нет glibc, alsa-lib и `/system/vendor/quasar`. Демон
  собирается под `aarch64-linux-android` (bionic, NDK).
- devicetree: `model = "Amlogic"`, `compatible = "amlogic, g12a"`, без `yandex,*`. Модель
  определяется по `ro.yndx.quasar.platform` = `yandexstation_2`; ревизия с Zigbee — по узлу
  `/proc/device-tree/zigbee`.
- `/system` и `/vendor` под dm-verity, `/odm` — нет: root-метод владельца живёт в `/odm`
  (`/odm/bin`, init-сервисы в `/odm/etc/init/*.rc`). Свой автозапуск — тоже init-сервисом в
  `/odm/etc/init`; файлы root-метода не трогать.
- SELinux на станции владельца — Permissive (так выставляет root-метод).
- absshd не поддерживает scp/sftp: файлы — через `ssh ... 'cat > файл' < файл`.

## Стек Яндекса

- Весь quasar — приложение `com.yandex.io.sdk` (`/system/priv-app/ServicesIosdk`), нативная часть —
  `lib/arm64-v8a/libquasar_daemons.so` внутри APK (несжатая, грузится прямо из APK).
- `pm disable` отказывает (`Cannot disable a protected package`), `am force-stop` процесс не
  убивает; после `kill` его сразу поднимают привязки (`tv.home`, `tv.alice`, `tv.services` и др.).
  Device owner — `com.yandex.tv.services.platform`.
- Микрофон у `io.sdk` отбирается обратимо: `cmd appops set com.yandex.io.sdk RECORD_AUDIO ignore`
  и `kill` процесса — после перезапуска `audioserver` пишет `Request denied by app op: 27`, вход
  свободен. Возврат — `RECORD_AUDIO allow` и снова `kill`.
- Android 9 не даёт двух захватов одновременно: пока пишет `io.sdk`, наш `startRecording` не
  пройдёт.
- Спотер («Алиса», командный) статически влинкован в `libquasar_daemons.so`; символы `spotter_*`
  не экспортированы (есть только строки сообщений). Экспортированы JNI-методы
  `JniAudioSourceNativeMethods_doPushAudioBuffer`, `SpotterServiceProviderNativeMethods_get`,
  `SpotterListenerPluginNativeMethods_registerListeners`.
- `libquasar_gstreamer.so` из того же APK собрана под bionic: `playbin`, `uridecodebin`,
  `souphttpsrc`, `hlsdemux`, `flacdec`, `opusdec`, `qtdemux`, `wavparse`, `appsink`, `appsrc`,
  `openslessink`, `openslessrc`.

## Звук

- ALSA держит HAL `audioserver`; прямой доступ к `/dev/snd` не используется.
- Захват: `AudioRecord`, источник `MIC`, 48 кГц, s16, `setChannelIndexMask(0xffff)` — 16 каналов,
  как у `io.sdk`. Работает из root-процесса `app_process`, если подставить `ActivityThread` с
  `opPackageName` (иначе JNI Android 9 падает на null).
- VQE Яндекса — preprocessing-эффект `audioserver` для источника `mic`
  (`/vendor/etc/audio_effects.xml`, `/vendor/lib64/soundfx/libyandexvqe.so`); он применяется и к
  нашему захвату. Библиотека VQE (`/vendor/lib64/libyandex_vqe_64.so`) — другой ABI, чем
  `libyandex_evil.so` Buildroot-станций: `YandexVqeC_Init`, `YandexVqeC_ProcessAudio`,
  `YandexVqeC_SendProtobufCommand`.
- Каналы захвата (тон 440 Гц амплитудой 0.05 на динамике и речь с 1-2 м; уровни — RMS в дБFS,
  тон — RMS его составляющей):

  | Канал | Содержимое |
  |---|---|
  | 0 | канал 1 × 30 (`omniChannelGainCoefficient` пресета, +29.5 дБ); пик речи −15 дБFS |
  | 1 | выход VQE `vqe_0`: речь −62 (на микрофонах −60), тон −128 (на микрофонах −69) — эхо подавлено на ~57 дБ; шум −91 |
  | 2 | `vqe_bnr`: речь ослаблена сильнее шума |
  | 3-10 | сырые микрофоны: шум −81, речь −60, тон −69 |
  | 11-12 | аппаратная петля: тон −83, речи нет |
  | 13-14 | цифровой опорный сигнал динамиков L/R: тон −33 при громкости 20/100 |
  | 15 | нули |

  В `quasar.cfg` `io.sdk` каналы распознавания и спотера — `vqe`.
- В первые ~0.5 с после старта захвата — щелчки до 0 дБFS на всех каналах.
- Воспроизведение: `AudioTrack` из root `app_process` работает. Громкость `STREAM_MUSIC` 0..100
  (у владельца 20).

## Светодиоды и экран

- Кольцо — `/dev/graphics/fb5`, 24×1, RGB24, порядок байтов R, G, B; кадр — простая запись 72
  байт в устройство. Позиция светодиода 0 и направление обхода не определены.
- Экран — `/dev/graphics/fb4`, 25×16, RGB24, stride 75; строка 0 — верхняя; кадр — простая запись
  1200 байт. `io.sdk` рисует часы прямо в `fb4` (содержимое меняется раз в минуту) и держит `fb4`,
  `fb5`, `/dev/input/event{0,2,3}` и `/dev/lp_uart` открытыми.

## Проверено

- Захват 16 каналов с VQE из своего процесса, раскладка каналов (тон и речь), 12 ноября 2026.
- Воспроизведение `AudioTrack` (тон амплитудой 0.05 при громкости 20/100).
- Запись в `fb5` и `fb4` видна на станции.
- Отбор микрофона у `io.sdk` через appops и возврат.

## Не проверено

- Вывод через AAudio из нативного процесса и `libquasar_gstreamer.so` из APK.
- Кнопки и крутилка громкости, датчик освещённости.
- Zigbee (EZSP на `/dev/lp_uart`).
- Автозапуск из `/odm/etc/init`.
