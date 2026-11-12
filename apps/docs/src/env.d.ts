declare module 'virtual:icons' {
  /** The SVG body of each icon in src/icons.ts, with its view box. */
  const icons: Record<string, { body: string, width: number, height: number }>
  export default icons
}

declare module '*.svg' {
  const url: string
  export default url
}

declare module '*.png' {
  const url: string
  export default url
}

declare module 'virtual:cjk-font' {}
