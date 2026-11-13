import type { Component } from 'vue'

import Chaos from './Chaos.vue'
import Cluster from './Cluster.vue'
import Config from './Config.vue'
import Drivers from './Drivers.vue'
import Files from './Files.vue'
import GettingStarted from './GettingStarted.vue'
import Hero from './Hero.vue'
import Identity from './Identity.vue'
import Network from './Network.vue'
import Routing from './Routing.vue'
import Separation from './Separation.vue'

/** The scenes, by the id that `?capture=<id>`, the banner script and src/scenes/sizes.ts use. */
export const SCENES: Readonly<Record<string, Component>> = {
  'hero': Hero,
  'separation': Separation,
  'cluster': Cluster,
  'identity': Identity,
  'drivers': Drivers,
  'config': Config,
  'routing': Routing,
  'network': Network,
  'files': Files,
  'chaos': Chaos,
  'getting-started': GettingStarted,
}
