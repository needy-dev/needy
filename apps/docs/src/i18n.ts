import { computed, ref, watch } from 'vue'

// The page speaks English and Simplified Chinese, as the injeca Docs do. `?lang=` wins, then the viewer's choice,
// then the browser. The README banners are captured in English.
// NOTICE: a small helper instead of vue-i18n: every text has its translation next to it (`tr({ en, zh })`), so a
// scene keeps its words in one place and nothing needs message keys.

export type Locale = 'en' | 'zh-Hans'

/** A text in both languages. */
export interface Text {
  en: string
  zh: string
}

const KEY = 'needy:lang'

function initial(): Locale {
  const forced = new URLSearchParams(location.search).get('lang')
  if (forced === 'en' || forced === 'zh-Hans')
    return forced
  if (new URLSearchParams(location.search).has('capture'))
    return 'en'
  try {
    const saved = localStorage.getItem(KEY)
    if (saved === 'en' || saved === 'zh-Hans')
      return saved
  }
  catch {}
  return navigator.language.startsWith('zh') ? 'zh-Hans' : 'en'
}

export const locale = ref<Locale>(initial())

watch(locale, (value) => {
  document.documentElement.lang = value
  try {
    localStorage.setItem(KEY, value)
  }
  catch {}
}, { immediate: true })

/** The text in the page's language. A plain string has no translation and shows as it is. */
export function tr(text: Text | string | undefined): string {
  if (text === undefined || typeof text === 'string')
    return text ?? ''
  return locale.value === 'zh-Hans' ? text.zh : text.en
}

export const isZh = computed(() => locale.value === 'zh-Hans')
