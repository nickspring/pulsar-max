# third_party

`pulsar-lan` — копия `satellite/crates/pulsar-lan` из [pulsar](https://github.com/Morvex885/pulsar)
тега `v0.6.1` с исправлением сборки под `aarch64-linux-android`:

- `Cargo.toml`: `pulsar-core` из этого workspace вместо соседнего пути
- `src/mdns.rs`: свои `RTMGRP_LINK` / `RTMGRP_IPV4_IFADDR` (в libc для Android их нет) и проверки
  раскладки `ifinfomsg` только там, где libc эту структуру знает

Копию и запись `[patch]` в `Cargo.toml` удалить, когда исправление попадёт в тег pulsar.
