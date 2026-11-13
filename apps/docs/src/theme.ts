import { ref, watch } from 'vue'

// After the injeca Docs: `?theme=` wins (the banner script sets it), then the viewer's choice, then the system.
// Source: https://github.com/moeru-ai/injeca/blob/a361694ff67b4f2b2feb4b4cf0bf585e0f328a68/apps/docs/src/theme.ts

export type ThemeName = 'dark' | 'light'

const KEY = 'needy:theme'

function stored(): ThemeName | undefined {
  try {
    const value = localStorage.getItem(KEY)
    return value === 'dark' || value === 'light' ? value : undefined
  }
  catch {
    return undefined
  }
}

const system = window.matchMedia('(prefers-color-scheme: dark)')
const forced = new URLSearchParams(location.search).get('theme')

/** The theme of the page. index.html sets the attribute before the first paint, so the page never flashes. */
export const theme = ref<ThemeName>(forced === 'light' || forced === 'dark' ? forced : stored() ?? (system.matches ? 'dark' : 'light'))

watch(theme, (value) => {
  document.documentElement.dataset.theme = value
}, { immediate: true })

system.addEventListener('change', (event) => {
  if (!forced && !stored())
    theme.value = event.matches ? 'dark' : 'light'
})

export function toggleTheme(): void {
  theme.value = theme.value === 'dark' ? 'light' : 'dark'
  try {
    localStorage.setItem(KEY, theme.value)
  }
  catch {}
}
