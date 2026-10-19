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
 * Operations for a selection (one mime per file): batch operations first (e.g. "Merge PDFs"),
 * then the per-file operations every selected type offers.
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
  return ops;
}
