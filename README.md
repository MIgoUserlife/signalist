# Signalist

Компактний десктопний застосунок для одночасної роботи з Telegram і WhatsApp Web. Вузька бічна панель (72 px) із перемикачем месенджерів, бейджами непрочитаних повідомлень і системними сповіщеннями — без зайвого інтерфейсу.

Побудовано на **Tauri 2 + Vue 3 + TypeScript**.

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
- [Додавання нового месенджера](#додавання-нового-месенджера)

---

## Вимоги

| Інструмент | Версія |
|---|---|
| [Rust](https://rustup.rs/) | stable |
| [Node.js](https://nodejs.org/) | 20+ |
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
│   ├── src/                    # Vue 3 + TypeScript (бічна панель)
│   │   ├── App.vue             # головний компонент: панель, перемикач, налаштування
│   │   └── style.css           # Tailwind v4 + Catppuccin-палітра (@theme)
│   └── src-tauri/
│       ├── src/lib.rs          # вся логіка Rust: webview, команди, трей
│       ├── inject/             # JS-скрипти для Telegram і WhatsApp webview
│       ├── capabilities/       # дозволи Tauri (IPC)
│       └── tauri.conf.json     # конфігурація застосунку, версія, updater
├── .github/
│   └── workflows/
│       └── release.yml         # CI/CD: автоматична збірка та публікація релізів
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

1. Перейдіть до [Releases](https://github.com/MIgoUserlife/signalist/releases) на GitHub.
2. Завантажте актуальний інсталятор для вашої платформи:
   - **macOS** — `Signalist_x.x.x_universal.dmg`
   - **Windows** — `Signalist_x.x.x_x64-setup.exe`
   - **Linux** — `signalist_x.x.x_amd64.AppImage`
3. Встановіть звичайним способом для вашої ОС.

Після встановлення застосунок самостійно перевірятиме наявність нових версій при кожному запуску.

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

**1. Оновіть версію** у `signalist/src-tauri/tauri.conf.json`:

```json
{
  "version": "0.2.0"
}
```

**2. Закомітьте та створіть тег:**

```sh
git add signalist/src-tauri/tauri.conf.json
git commit -m "chore: bump version to 0.2.0"
git tag v0.2.0
git push origin main --tags
```

**3. GitHub Actions** автоматично:
- зберає застосунок для macOS (universal), Windows і Linux
- підписує артефакти приватним ключем
- публікує **Draft Release** з усіма файлами та `latest.json`

**4. Опублікуйте Release** на GitHub: перейдіть у Releases → знайдіть чернетку → натисніть **Publish release**.

Після публікації `latest.json` стане доступним за endpoint-ом updater'а і всі запущені застосунки отримають сповіщення про оновлення.

---

## Автоматичне оновлення в застосунку

При кожному запуску Signalist у фоні перевіряє наявність нової версії на GitHub Releases.

Якщо оновлення знайдено — у нижній частині бічної панелі з'являється кнопка **UPD** (блакитна, зі стрілкою вниз). Натисніть її, щоб завантажити та встановити оновлення. Застосунок автоматично перезапуститься.

```
┌──────┐
│ logo │
│ v0.1 │
├──────┤
│  TG  │
│  WA  │
├──────┤
│ auto │
│ key  │
│ [↓]  │  ← з'являється при наявності оновлення
└──────┘
```

Оновлення підписані криптографічно — встановлюються лише артефакти, підписані вашим приватним ключем.

---

## Зміна логотипу

Логотип застосунку присутній у двох місцях:

**1. Іконка у бічній панелі** (`src/assets/logo.png`):
- замініть файл на власне зображення (рекомендований розмір: 512×512 px, PNG)
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

## Додавання нового месенджера

1. **`src-tauri/src/lib.rs`** — додайте запис у масив `MESSENGERS` з унікальним `data_store_id`.
2. **`src-tauri/inject/<name>.js`** — створіть скрипт інʼєкції за зразком `telegram.js` або `whatsapp.js` (debounce + fetch-patch).
3. **`src-tauri/src/lib.rs`** — у функції `open_messenger` додайте гілку `match` для завантаження нового inject-скрипту.
4. **`src/App.vue`** — додайте обʼєкт у масив `messengers` з `label`, `displayName` та іконкою з `lucide-vue-next`.
5. **`src-tauri/capabilities/messenger-ipc.json`** — додайте новий webview label і дозволені URL.
