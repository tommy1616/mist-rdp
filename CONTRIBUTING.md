# 貢獻指南 · Contributing

## 新增或修改語言 · Adding a language

介面文字集中在 `frontend/locales/`，不要把文字直接寫死在 HTML 或 JS 裡。
UI strings live in `frontend/locales/`; do not hard-code text in HTML or JS.

1. 複製 `frontend/locales/en.js` 為 `frontend/locales/<語言代碼>.js`（例如 `ja.js`），把 `window.I18N['en']` 改成新的語言代碼，翻譯所有值。
   Copy `en.js`, change `window.I18N['en']` to your language code and translate every value.
2. 在 `frontend/index.html` 加上 `<script src="locales/<code>.js"></script>`，並在 `<select id="settingsLang">` 加入一個 `<option>`。
   Add the script tag and an `<option>` in `#settingsLang`.
3. 在 `frontend/i18n.js` 的 `LANGS` 陣列加入該語言代碼。
   Add the code to `LANGS` in `i18n.js`.

缺少的 key 會自動退回繁體中文（`zh-TW`）。
Missing keys fall back to Traditional Chinese (`zh-TW`).

### 在程式中使用 · Usage

- 靜態文字：HTML 元素加上 `data-i18n="key"`（文字）、`data-i18n-title`、`data-i18n-placeholder`、`data-i18n-aria-label`。
- 動態文字：`tr('key')`，帶參數用 `tr('key', { name })`，字串內以 `{name}` 表示。
- 新增 key 時，請同時更新 `zh-TW.js` 與 `en.js`。

目前後端（Rust）回傳的錯誤訊息尚未翻譯，仍為原文。
Error messages returned by the Rust backend are not translated yet.

## 開發 · Development

```bash
cd src-tauri
cargo test --bins
node --check ../frontend/app.js ../frontend/vault-ui.js ../frontend/app-security.js ../frontend/i18n.js
```

請勿提交真實的保管庫、WebDAV 憑證或個人主機位址；截圖與測試請使用虛構資料（例如 RFC 5737 位址 `192.0.2.x`）。
Never commit real vaults, WebDAV credentials or personal host addresses; use fictional data (for example RFC 5737 `192.0.2.x`) in tests and screenshots.
