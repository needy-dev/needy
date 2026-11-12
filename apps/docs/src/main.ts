import { createApp } from 'vue'

import App from './App.vue'

import '@fontsource/varela-round/400.css'
import '@fontsource-variable/dm-sans'
import '@fontsource/maple-mono/400.css'
import '@fontsource/maple-mono/500.css'
import './style.css'
import './kit/focus.css'
// LXGW 975 Yuan for the Chinese text, cut down to its characters at build time.
import 'virtual:cjk-font'

createApp(App).mount('#app')
