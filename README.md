# Signalist

Компактний десктопний застосунок для роботи з Telegram, WhatsApp Web та будь-якими веб-сервісами в одному вікні. Вузька бічна панель (64 px) з перемикачем між джерелами, бейджами непрочитаних повідомлень і системними сповіщеннями — без зайвого інтерфейсу.

Побудовано на **Tauri 2 + Vue 3 + TypeScript**.

---

## Функціональність

- **Telegram і WhatsApp Web** — вбудовані як дочірні webview з окремими data store (незалежні сесії)
- **Каталог месенджерів** — додавайте Instagram, Facebook, Discord, Slack, Signal, LinkedIn, X одним кліком; зберігаються між сесіями
- **Кастомні веб-шорткати** — будь-який сайт (Notion, Linear, Claude тощо); підтримується іконка зі списку або перша літера назви
- **Бейджи непрочитаних** — лічильник зчитується з DOM/заголовка сторінки і відображається в панелі та трей-іконці
- **Системні сповіщення** — push-сповіщення при нових повідомленнях (з cooldown 5 с)
- **Глобальний хоткей** — Show/Hide вікна без мишки (за замовчуванням `⌘⇧S`, змінюється в налаштуваннях)
- **Теми сайдбару** — чотири режими: SYS / LITE / DARK / AUTO (авто-детекція фону активного месенджера); macOS Vibrancy для frosted-glass ефекту
- **Автозапуск при вході** — вмикається/вимикається в налаштуваннях
- **Показ/приховання у Dock** — режим menu-bar-only через трей-меню
- **Автоматичне оновлення** — перевіряє GitHub Releases при кожному запуску

---

## Зміст

- [Вимоги](#вимоги)
- [Структура проєкту](#структура-проєкту)
- [Розробка](#розробка)
- [Збірка](#збірка)
- [Встановлення на інший комп'ютер](#встановлення-на-інший-компютер)
- [Випуск нової версії (Release)](#випуск-нової-версії-release)
- [Автоматичне оновлення в застосунку](#автоматичне-оновлення-в-застосунку)
- [Зміна логотипу](#зміна-логотипу)
- [Додавання вбудованого месенджера](#додавання-вбудованого-месенджера)
- [Changelog](#changelog)

---

## Вимоги

| Інструмент | Версія |
|---|---|
| [Rust](https://rustup.rs/) | stable |
| [Node.js](https://nodejs.org/) | 22+ |
| [Tauri CLI](https://tauri.app/start/) | v2 (входить у `devDependencies`) |

На macOS додатково потрібен Xcode Command Line Tools:
```sh
xcode-select --install
```

---

## Структура проєкту

```
Signalist/                      # корінь git-репозиторію
├── signalist/                  # Tauri-проєкт
│   ├── src/                    # Vue 3 + TypeScript (бічна панель + діалоги)
│   │   ├── App.vue             # views: sidebar, add-shortcut, edit-shortcut, add-messenger
│   │   ├── messengerCatalog.ts # каталог сервісів для додавання (Instagram, Discord тощо)
│   │   ├── assets/icons/       # SVG-іконки для шорткатів і месенджерів
│   │   └── style.css           # Tailwind v4 + Catppuccin-палітра (@theme)
│   └── src-tauri/
│       ├── src/lib.rs          # вся логіка Rust: webview, команди, трей
│       ├── inject/
│       │   ├── telegram.js     # JS-ін'єкція для Telegram webview
│       │   ├── whatsapp.js     # JS-ін'єкція для WhatsApp webview
│       │   └── shortcut.js     # JS-ін'єкція для кастомних webview (anti-bot)
│       ├── capabilities/       # дозволи Tauri (IPC)
│       └── tauri.conf.json     # конфігурація застосунку, версія, updater
├── .github/
│   └── workflows/
│       └── release.yml         # CI/CD: автоматична збірка та публікація релізів
├── CHANGELOG.md
└── README.md
```

---

## Розробка

```sh
cd signalist
npm install
npm run tauri dev
```

Vite запускається на `http://localhost:1420`, Rust перекомпілюється автоматично при змінах.

Корисні команди:

```sh
# лише TypeScript-перевірка + bundling фронтенду
npm run build

# перевірка Rust без повної збірки
cd src-tauri && cargo check
```

> **Примітка щодо dev-режиму.** DevTools для месенджер-webview доступні лише в debug-збірці (`npm run tauri dev`). У production вони вимкнені. Логи Rust виводяться у термінал лише при `#[cfg(debug_assertions)]`.

---

## Збірка

Локальна production-збірка (без публікації):

```sh
cd signalist
npm run tauri build
```

Готовий `.app` / `.dmg` буде в `signalist/src-tauri/target/release/bundle/`.

> Для підписаної збірки з підтримкою оновлень потрібен приватний ключ (див. [Випуск нової версії](#випуск-нової-версії-release)).

---

## Встановлення на інший комп'ютер

Наразі публікуються лише збірки для **macOS на Apple Silicon (arm64)**.

1. Перейдіть до [Releases](https://github.com/MIgoUserlife/signalist/releases) на GitHub.
2. Завантажте актуальний `Signalist_x.x.x_aarch64.dmg`.
3. Перетягніть `Signalist.app` у `/Applications`.

### Швидкий фікс: «Програму пошкоджено, її не можна відкрити»

Збірки **не нотаризовані** Apple, тож macOS Gatekeeper при першому запуску показує помилку про пошкодження. Це не справжнє пошкодження — це quarantine-атрибут, доданий браузером при завантаженні. Зніміть його одною командою:

```sh
xattr -cr /Applications/Signalist.app
```

Після цього застосунок запуститься нормально. Команду треба повторити лише при наступному ручному завантаженні `.dmg`. Авто-оновлення цього не потребують — застосунок сам качає файли в обхід quarantine.

Після встановлення Signalist самостійно перевірятиме наявність нових версій при кожному запуску.

---

## Випуск нової версії (Release)

### Перший запуск: підготовка підпису (одноразово)

**1. Згенеруйте ключі підпису:**

```sh
cd signalist
npm run tauri -- signer generate -w ~/.tauri/signalist.key
```

У терміналі буде виведено публічний ключ у форматі:

```
Public key: dW50cnVzdGVkIGNvbW1...
```

**2. Вставте публічний ключ у `signalist/src-tauri/tauri.conf.json`:**

```json
"plugins": {
  "updater": {
    "pubkey": "ВАШ_ПУБЛІЧНИЙ_КЛЮЧ",
    ...
  }
}
```

**3. Додайте приватний ключ до GitHub Secrets** (Settings → Secrets and variables → Actions):

| Secret | Значення |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | вміст файлу `~/.tauri/signalist.key` |

---

### Публікація нової версії

**1. Оновіть версію** у `signalist/package.json`, `signalist/src-tauri/tauri.conf.json` та `signalist/src-tauri/Cargo.toml`.

**2. Закомітьте, створіть тег і запушіть:**

```sh
git add signalist/package.json signalist/src-tauri/tauri.conf.json signalist/src-tauri/Cargo.toml
git commit -m "chore: bump version to x.x.x"
git tag vx.x.x
git push origin main
git push origin vx.x.x
```

**3. GitHub Actions** автоматично:
- збирає застосунок для macOS на Apple Silicon (`aarch64-apple-darwin`)
- підписує артефакти приватним ключем
- публікує реліз із `.dmg`, `.app.tar.gz`, `.app.tar.gz.sig` і `latest.json`

Реліз публікується **одразу, не як чернетка** — інакше `tauri-action` пропускає крок генерації `latest.json`, і авто-оновлення не працює.

> **Важливо:** для генерації updater-артефактів (`.tar.gz`/`.sig`) у `tauri.conf.json` має бути `bundle.createUpdaterArtifacts: true`. У Tauri 2.x без цього прапора bundler випускає лише `.app` і `.dmg`.

---

## Автоматичне оновлення в застосунку

При кожному запуску Signalist у фоні перевіряє наявність нової версії на GitHub Releases.

Якщо оновлення знайдено — у нижній частині бічної панелі з'являється кнопка **UPD** (зі стрілкою вниз). Натисніть її, щоб завантажити та встановити оновлення. Застосунок автоматично перезапуститься.

Оновлення підписані криптографічно — встановлюються лише артефакти, підписані вашим приватним ключем.

---

## Зміна логотипу

Логотип застосунку присутній у двох місцях:

**1. Іконка у бічній панелі** (`src/assets/logo.svg`):
- замініть файл на власне SVG-зображення
- Vue автоматично підхопить зміну при наступній збірці

**2. Іконки системи** (трей, Dock, інсталятор) у `src-tauri/icons/`:
- підготуйте вихідне зображення 1024×1024 px (PNG, прозорий фон)
- згенеруйте всі розміри автоматично:

```sh
cd signalist
npm run tauri icon /шлях/до/вашого/logo-1024.png
```

Команда перезапише всі файли в `src-tauri/icons/` потрібними розмірами для macOS, Windows і Linux.

> Після зміни іконок перезберіть застосунок (`npm run tauri build`) — системний кеш іконок може зберігати стару версію до перезапуску ОС.

---

## Додавання вбудованого месенджера

Цей розділ стосується додавання **вбудованого** месенджера з підтримкою бейджів непрочитаних (як Telegram/WhatsApp). Для звичайних сервісів без лічильника — використовуйте кнопку "+" у бічній панелі.

1. **`src-tauri/src/lib.rs`** — додайте запис у масив `MESSENGERS` з унікальним `data_store_id` та `display_name`.
2. **`src-tauri/inject/<name>.js`** — створіть скрипт ін'єкції за зразком `telegram.js` або `whatsapp.js` (debounce + fetch-patch для Tauri IPC).
3. **`src-tauri/src/lib.rs`** — у функції `open_messenger` додайте гілку `match` для завантаження нового inject-скрипту.
4. **`src/App.vue`** — додайте об'єкт у масив `messengers` з полями `label`, `displayName` та `icon`.
5. **`src-tauri/capabilities/messenger-ipc.json`** — додайте новий webview label і дозволені URL.

---

## Changelog

Детальна історія змін — у [CHANGELOG.md](./CHANGELOG.md).
