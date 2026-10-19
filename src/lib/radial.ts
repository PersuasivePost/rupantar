// Pure geometry for the radial menu. Angles are radians, 0 = straight up, increasing clockwise.

export const TAU = Math.PI * 2;

export const angleFromUp = (dx: number, dy: number): number => {
  const a = Math.atan2(dx, -dy);
  return a < 0 ? a + TAU : a;
};

/** Offset from the center for a radius and angle (screen coordinates, y down). */
export const polar = (r: number, a: number) => ({ x: r * Math.sin(a), y: -r * Math.cos(a) });

/** Wedge `i` is centered on `i * TAU / n`, so wedge 0 points up. */
export const pickWedge = (angle: number, n: number): number => Math.round(angle / (TAU / n)) % n;

/**
 * Which of `k` equal slices of the arc [start, start + span] the angle falls in.
 * Angles outside the arc snap to the nearer end, so flicking past the edge still selects.
 */
export function pickInSpan(angle: number, start: number, span: number, k: number): number {
  let rel = (((angle - start) % TAU) + TAU) % TAU;
  if (rel > span) rel = rel - span < TAU - rel ? span - 1e-6 : 0;
  return Math.min(k - 1, Math.floor((rel / span) * k));
}

/** SVG path for an annular sector, inset by `gap` px on each side so wedges are visibly separated. */
export function sectorPath(
  c: number,
  rIn: number,
  rOut: number,
  a0: number,
  a1: number,
  gap: number,
): string {
  const o0 = a0 + gap / rOut;
  const o1 = a1 - gap / rOut;
  const i0 = a0 + gap / rIn;
  const i1 = a1 - gap / rIn;
  const pt = (r: number, a: number) => `${(c + r * Math.sin(a)).toFixed(2)} ${(c - r * Math.cos(a)).toFixed(2)}`;
  const large = o1 - o0 > Math.PI ? 1 : 0;
  return [
    `M ${pt(rOut, o0)}`,
    `A ${rOut} ${rOut} 0 ${large} 1 ${pt(rOut, o1)}`,
    `L ${pt(rIn, i1)}`,
    `A ${rIn} ${rIn} 0 ${large} 0 ${pt(rIn, i0)}`,
    "Z",
  ].join(" ");
}
