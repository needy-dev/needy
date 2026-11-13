/** The middle of line `i` of a Profile.vue card whose top is at `top`: 10px above the lines, 30px with a title, 20px a line. */
export function profileLineY(top: number, i: number, titled = true): number {
  return top + (titled ? 30 : 10) + i * 20 + 10
}

/** The height of a Profile.vue card of `lines` lines. */
export function profileHeight(lines: number, titled = true): number {
  return (titled ? 30 : 10) + lines * 20 + 12
}
