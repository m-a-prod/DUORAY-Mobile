# DUORAY для телефонов

Android-версия [DUORAY](https://github.com/m-a-prod/DUORAY), клиента DUALIZM VPN
на [Xray-core](https://github.com/XTLS/Xray-core). iOS — следующий шаг, на той же основе.

- **Интерфейс как на ПК**: та же палитра, элементы и логика подписок (Slint, Rust).
- **Ядро DUORAY** (`duoray-core`): подписки, ссылки, конфиги xray, маршрутизация.
- **Сервер по сети**: свой сервер для Wi-Fi, для мобильной сети и для отдельных
  Wi-Fi сетей; при смене сети подключение само переходит на нужный сервер.
- **Xray-core без посредников**: официальная сборка xray под Android, его собственный
  `tun`-вход читает TUN из VpnService (`xray.tun.fd`). Приложение исключено из
  своего VPN, поэтому соединения xray с серверами идут напрямую.

## Как устроено

```
Kotlin (android/)                 Rust (src/)
MainActivity : NativeActivity ──► android_main → Slint UI (ui/app.slint, app.rs)
DuorayVpnService : VpnService ──► Native.tunUp(fd) → service.rs → xray с fd TUN (tunnel.rs)
NetworkWatcher ─────────────────► Native.networkChanged → правило сетей (netpolicy.rs)
```

## Сборка

Рядом должен лежать репозиторий DUORAY (`../DUORAY`): из него берётся `duoray-core`.

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
export ANDROID_HOME=~/Android/Sdk ANDROID_NDK_HOME=~/Android/Sdk/ndk/<версия>
scripts/build-apk.sh debug        # или release
```

Скрипт скачивает последний релиз Xray-core, собирает Rust-библиотеку и APK.
На GitHub то же самое делает `.github/workflows/android.yml`.

Экраны можно смотреть без телефона: `cargo run --bin duoray-preview`
(VPN там только изображает подключение).

## Лицензия

GPL-3.0, как DUORAY. Флаги — Twemoji (CC-BY 4.0), см. `assets/NOTICE.md`.
