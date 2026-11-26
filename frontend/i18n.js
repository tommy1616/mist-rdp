// Minimal i18n: dictionaries live in locales/<lang>.js (window.I18N[lang]).
// To add a language: copy locales/en.js, translate the values, load it in index.html
// and add it to LANGS below plus the <select id="settingsLang"> options.
(() => {
  const FALLBACK = 'zh-TW';
  const LANGS = ['zh-TW', 'en'];
  const KEY = 'mistrdp.lang';
  const saved = localStorage.getItem(KEY);
  let lang = LANGS.includes(saved) ? saved : (/^zh/i.test(navigator.language) ? 'zh-TW' : 'en');

  window.tr = (key, params) => {
    let s = (window.I18N[lang] || {})[key] ?? window.I18N[FALLBACK][key] ?? key;
    if (params) s = s.replace(/\{(\w+)\}/g, (m, k) => (k in params ? params[k] : m));
    return s;
  };

  function apply() {
    document.documentElement.lang = lang === 'zh-TW' ? 'zh-Hant' : lang;
    document.title = 'MistRDP';
    for (const el of document.querySelectorAll('[data-i18n]')) el.textContent = tr(el.dataset.i18n);
    for (const attr of ['title', 'placeholder', 'aria-label']) {
      for (const el of document.querySelectorAll(`[data-i18n-${attr}]`)) el.setAttribute(attr, tr(el.getAttribute(`data-i18n-${attr}`)));
    }
  }

  window.getLang = () => lang;
  window.setLang = (next) => {
    if (!LANGS.includes(next) || next === lang) return;
    lang = next;
    localStorage.setItem(KEY, lang);
    apply();
    window.dispatchEvent(new Event('i18n-changed'));
  };

  apply();
  const picker = document.getElementById('settingsLang');
  if (picker) {
    picker.value = lang;
    picker.addEventListener('change', () => window.setLang(picker.value));
  }
})();
