/** A horizontal S-curve from one point to another, the way a wire leaves one box and enters the next. */
export function curve(x1: number, y1: number, x2: number, y2: number): string {
  const bend = Math.max(30, Math.abs(x2 - x1) / 2)
  const dir = x2 >= x1 ? 1 : -1
  return `M${x1} ${y1} C${x1 + bend * dir} ${y1} ${x2 - bend * dir} ${y2} ${x2} ${y2}`
}

/** A straight line. */
export function line(x1: number, y1: number, x2: number, y2: number): string {
  return `M${x1} ${y1} L${x2} ${y2}`
}

/** A vertical S-curve, for a wire that leaves a box from its bottom. */
export function drop(x1: number, y1: number, x2: number, y2: number): string {
  const bend = Math.max(24, Math.abs(y2 - y1) / 2)
  return `M${x1} ${y1} C${x1} ${y1 + bend} ${x2} ${y2 - bend} ${x2} ${y2}`
}
