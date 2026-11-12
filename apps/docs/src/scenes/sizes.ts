// The design size of each scene, which is the size of its README banner. The file has no imports, so the banner
// script can read it in Node. Each keeps one ratio: 2.5:1 for the first screen, 2:1 for a concept that needs room,
// 3:1 for a figure inside a README section.

export const SIZES: Readonly<Record<string, { readonly width: number, readonly height: number }>> = {
  'hero': { width: 1440, height: 576 },
  'separation': { width: 1440, height: 720 },
  'cluster': { width: 1440, height: 720 },
  'identity': { width: 1440, height: 576 },
  'drivers': { width: 1440, height: 720 },
  'config': { width: 1440, height: 720 },
  'routing': { width: 1440, height: 480 },
  'network': { width: 1440, height: 480 },
  'files': { width: 1440, height: 480 },
  'chaos': { width: 1440, height: 576 },
  'getting-started': { width: 1440, height: 720 },
}
