# third_party

Копии крейтов [pulsar](https://github.com/Morvex885/pulsar) тега `v0.6.1` с правками под
`aarch64-linux-android`. Подключены через `[patch]` в `Cargo.toml`; копию и запись `[patch]`
удалить, когда правка попадёт в тег pulsar.

`pulsar-lan` — копия `satellite/crates/pulsar-lan`:

- `Cargo.toml`: `pulsar-core` из этого workspace вместо соседнего пути
- `src/mdns.rs`: свои `RTMGRP_LINK` / `RTMGRP_IPV4_IFADDR` (в libc для Android их нет) и проверки
  раскладки `ifinfomsg` только там, где libc эту структуру знает

`pulsar-sendspin` — копия `satellite/crates/pulsar-sendspin` без `src/sink.rs`, `src/gain.rs` и
`src/bin/` (вывод в ALSA через `pulsar-alsa`, которого на Android нет):

- `src/lib.rs`: трейт `AudioOutput` и структура `Written`; `SendspinConfig.output` вместо
  `SendspinConfig.model`
- `src/service.rs`: звук идёт в `AudioOutput`; время конца звучания приходит от вывода как
  задержка от текущего момента и переводится в часы библиотеки здесь
- `build.rs`: на Android — статические `c++_static` и `c++abi` вместо `libc++_shared.so`, которой
  на станции нет; путь к `CMakeLists.txt` в комментарии
- `Cargo.toml`: зависимости из этого workspace, без `pulsar-alsa`

`sendspin-cpp/` — `fetch.sh` и `CMakeLists.txt` из `satellite/third_party` pulsar (те же
закреплённые коммиты и роли). В `CMakeLists.txt` добавлены минимальная версия CMake 3.18 и
отключение `_FORTIFY_SOURCE` для `noise-c/src/protocol/patterns.c`: NDK включает его по умолчанию,
а `noise_pattern_expand` передаёт `memchr` длину больше массива шаблона (конец шаблона внутри,
лишнего не читается), и bionic прерывает процесс по одной этой длине.

`make` скачивает исходники в `sendspin-cpp/src/` (вне git; заново, если изменился `fetch.sh`) и
собирает статические библиотеки CMake-тулчейном NDK в `build/sendspin-cpp/` (с нуля при смене
`CMakeLists.txt`, NDK или API).
