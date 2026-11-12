import type { Rule } from './rules'

// The profiles that the scenes show. Each is a profile from the README or examples/, cut to what a scene needs.

/** The rules of the routing scene and the first rules of the playground. */
export const ROUTING_RULES: readonly Rule[] = [
  { match: 'python3 **', to: 'build' },
  { match: 'git push **', deny: 'no pushes from the agent' },
  { match: '{xcodebuild,swift} **', to: 'mac' },
]

/** The KVM profile of the README's "Pick a profile", as the getting-started scene types it. */
export const DEMO_PROFILE = `user: { name: agent, uid: 1000, gid: 1000 }
actor:
  driver: linux-firecracker
  recipe: builtin:gce-ubuntu-22.04
  command: [bash]
executors:
  build: { driver: linux-container, key: "{run}" }
commands:
  - { match: "python3 **", to: build }`

/** examples/chaos/profile.yaml: its rule and faults. */
export const CHAOS_PROFILE = `commands:
  - { match: "{python3,curl} **", to: build }
chaos:
  seed: 1
  faults:
    broken-json: { file: { open: "**/*.json" }, errno: EIO }
    curl-down: { exec: "curl **", exit: 7, stderr: "curl: (7) …" }
    overheat: { sensor: { temp_c: 101 }, at: 2s }`
