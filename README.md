# pulsar-max

Голосовой спутник Home Assistant для Яндекс Станции Макс (`yandexstation_2`, Android 9 / YaOS).
Станция говорит с HA по ESPHome Native API, как станции на
[pulsar](https://github.com/Morvex885/pulsar); протоколы (ESPHome API, mDNS) берутся из крейтов
pulsar, а слой железа и демон написаны под Android.

pulsar поддерживает Buildroot-станции (Миди, Лайт 2, Мини…). Макс устроена иначе: Android 9, звук
через `audioserver`, VQE Яндекса — эффект Android, кольцо и экран — framebuffer. Подробности —
[docs/max.md](docs/max.md).

## Статус

В разработке. С Home Assistant на станции работают: обнаружение и подключение через ESPHome,
объявления (`assist_satellite.announce`) и диалог, начатый из HA
(`assist_satellite.start_conversation`): вопрос распознаётся, ответ звучит со станции. Ключевого
слова на станции, автозапуска и кнопок пока нет. План —
[docs/architecture.md](docs/architecture.md#этапы).

## Требования

- Яндекс Станция Макс с root через `/odm` (init-сервисы в `/odm/etc/init`), доступ по ssh.
- Прошивка, на которой проверялось: `5000000921`, `io.sdk` 3.367.25.

## Сборка

```sh
brew install rustup                   # тулчейн берётся из rust-toolchain.toml
brew install --cask android-ndk       # или NDK=<путь> для make
# помощники: JDK 17 и Android SDK (platforms/android-33, build-tools/36.0.0) в ANDROID_HOME
# или ~/Library/Android/sdk
make                                  # build/pulsar-max (aarch64-linux-android, API 28) и build/helper.dex
make check                            # fmt, clippy -D warnings, тесты max-core на хосте
```

## Структура

| Путь | Что |
|---|---|
| `crates/pulsar-max` | демон: ESPHome API, mDNS, контроллер, процессы-помощники, отбор микрофона у `io.sdk` |
| `crates/max-core` | логика без ввода-вывода: диалог, децимация 48 → 16 кГц, протокол помощников, медиа; тестируется на хосте |
| `android/helper` | помощники на Java (`app_process`): захват `AudioRecord` и плеер `MediaPlayer` |
| `third_party/pulsar-lan` | `pulsar-lan` с исправлением сборки под Android ([README](third_party/README.md)) |
| `probes/` | пробы железа: захват звука (Java через `app_process`), разбор записи |
| `docs/` | факты о станции и архитектура |

## Лицензия

AGPL-3.0-only, как у pulsar, на котором проект основан.
