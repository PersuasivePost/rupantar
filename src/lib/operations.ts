// Pure helpers over operation lists. `operationsFor` mirrors `Registry::operations_for` in
// crates/engine/src/lib.rs (the browser mocks use it); keep the two in step.
import type { Operation } from "../api";

/** What the menu renders: unavailable operations dropped, and parents left with no children. */
export function visibleOperations(ops: Operation[]): Operation[] {
  return ops.flatMap((op) => {
    if (op.available === false) return [];
    if (!op.children?.length) return [op];
    const children = visibleOperations(op.children);
    return children.length ? [{ ...op, children }] : [];
  });
}

/** Operations of `a` that `b` also offers (by id, recursively); available only if both are. */
export function intersectOperations(a: Operation[], b: Operation[]): Operation[] {
  return a.flatMap((op) => {
    const other = b.find((o) => o.id === op.id);
    if (!other) return [];
    const available = op.available !== false && other.available !== false;
    if (!op.children?.length) return [{ ...op, available }];
    const children = intersectOperations(op.children, other.children ?? []);
    return children.length ? [{ ...op, children, available }] : [];
  });
}

/**
 * Operations for a selection (one mime per file): the batch operations (e.g. "Merge") plus the
 * per-file operations every selected type offers, in priority order (ties: batch first).
 */
export function operationsFor(
  mimes: string[],
  perType: (mime: string) => Operation[],
  batch: (mimes: string[]) => Operation[],
): Operation[] {
  if (!mimes.length) return [];
  const [first, ...rest] = mimes;
  let common = perType(first);
  for (const m of new Set(rest)) if (m !== first) common = intersectOperations(common, perType(m));
  const ops = batch(mimes);
  for (const op of common) if (!ops.some((o) => o.id === op.id)) ops.push(op);
  return byPriority(ops);
}

/** Stable sort by priority; operations without one go last (mirrors `by_priority`). */
export function byPriority(ops: Operation[]): Operation[] {
  return ops
    .map((op, i) => ({ op, i }))
    .sort((a, b) => (a.op.priority ?? Infinity) - (b.op.priority ?? Infinity) || a.i - b.i)
    .map(({ op }) => op);
}

// ---- Menu layout --------------------------------------------------------------------------

/** Up to this many operations are all wedges; beyond it, the first five plus "More". */
export const MAX_WEDGES = 6;
/** Slots in an outer ring; longer lists are paged. */
export const RING_SLOTS = 8;
export const MORE_ID = "menu.more";
export const BACK_ID = "menu.back";
export const PAGE_ID = "menu.page";

/** The inner ring for a (visible, ordered) operation list. */
export function menuWedges(ops: Operation[]): Operation[] {
  if (ops.length <= MAX_WEDGES) return ops;
  const top = ops.slice(0, MAX_WEDGES - 1);
  return [...top, { id: MORE_ID, label: "More", icon: "Ellipsis", children: ops.slice(MAX_WEDGES - 1) }];
}

export type Slot =
  | { kind: "op"; op: Operation }
  /** Steps out of a drilled-in item. */
  | { kind: "back"; op: Operation }
  /** Goes to page `to`. */
  | { kind: "page"; op: Operation; to: number }
  /** Keeps paged rings the same shape on a short last page. */
  | { kind: "empty" };

/**
 * One page of an outer ring. `back` takes the first slot on every page. When the items don't
 * fit, the last slot turns the page (cycling) and every page keeps all slots, so the controls
 * stay put.
 */
export function ringPage(items: Operation[], page: number, back: boolean): { slots: Slot[]; page: number; pages: number } {
  const lead: Slot[] = back ? [{ kind: "back", op: { id: BACK_ID, label: "Back", icon: "Undo2" } }] : [];
  const room = RING_SLOTS - lead.length;
  const ops: Slot[] = items.map((op) => ({ kind: "op", op }));
  if (items.length <= room) return { slots: [...lead, ...ops], page: 0, pages: 1 };
  const per = room - 1;
  const pages = Math.ceil(items.length / per);
  const p = ((page % pages) + pages) % pages;
  const shown = ops.slice(p * per, p * per + per);
  const empty: Slot[] = Array.from({ length: per - shown.length }, () => ({ kind: "empty" }));
  const to = (p + 1) % pages;
  const turn: Slot = { kind: "page", to, op: { id: PAGE_ID, label: `Page ${to + 1} of ${pages}`, icon: "ChevronsRight" } };
  return { slots: [...lead, ...shown, ...empty, turn], page: p, pages };
}
