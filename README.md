# pulsar-max

Голосовой спутник Home Assistant для Яндекс Станции Макс (`yandexstation_2`, Android 9 / YaOS).
Станция говорит с HA по ESPHome Native API, как станции на
[pulsar](https://github.com/Morvex885/pulsar); протоколы (ESPHome API, mDNS) берутся из крейтов
pulsar, а слой железа и демон написаны под Android.

pulsar поддерживает Buildroot-станции (Миди, Лайт 2, Мини…). Макс устроена иначе: Android 9, звук
через `audioserver`, VQE Яндекса — эффект Android, кольцо и экран — framebuffer. Подробности —
[docs/max.md](docs/max.md).

## Статус

Ранний каркас. Работает: демон на станции, ESPHome API на порту 6053 и mDNS-объявление
`yandex-max-xxxxxx`. План — [docs/architecture.md](docs/architecture.md#этапы).

## Требования

- Яндекс Станция Макс с root через `/odm` (init-сервисы в `/odm/etc/init`), доступ по ssh.
- Прошивка, на которой проверялось: `5000000921`, `io.sdk` 3.367.25.

## Сборка

```sh
brew install rustup                   # тулчейн берётся из rust-toolchain.toml
brew install --cask android-ndk       # или NDK=<путь> для make
make                                  # build/pulsar-max (aarch64-linux-android, API 28)
make check                            # fmt, clippy -D warnings, тесты max-core на хосте
```

## Структура

| Путь | Что |
|---|---|
| `crates/pulsar-max` | демон: запуск, ESPHome API, mDNS |
| `crates/max-core` | логика без ввода-вывода, тестируется на хосте |
| `third_party/pulsar-lan` | `pulsar-lan` с исправлением сборки под Android ([README](third_party/README.md)) |
| `probes/` | пробы железа: захват звука (Java через `app_process`), разбор записи |
| `docs/` | факты о станции и архитектура |

## Лицензия

AGPL-3.0-only, как у pulsar, на котором проект основан.
