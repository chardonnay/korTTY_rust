// Determines whether the current snippet selection can be compared and orders
// the two selected snippets by their visible list order.
// Port of de.kortty.core.SnippetDiffSelectionSupport.

export interface SnippetDiffCandidate {
  id: string;
}

export interface SnippetDiffPair<T extends SnippetDiffCandidate> {
  left: T;
  right: T;
}

export function canDiff(selectedIds?: ReadonlySet<string> | null): boolean {
  return !!selectedIds && selectedIds.size === 2;
}

export function orderedPair<T extends SnippetDiffCandidate>(
  visibleSnippets: readonly T[] | null | undefined,
  selectedIds?: ReadonlySet<string> | null,
): SnippetDiffPair<T> | null {
  if (!canDiff(selectedIds) || !visibleSnippets || !selectedIds) {
    return null;
  }
  const ordered: T[] = [];
  for (const snippet of visibleSnippets) {
    if (selectedIds.has(snippet.id)) {
      ordered.push(snippet);
      if (ordered.length === 2) {
        return { left: ordered[0], right: ordered[1] };
      }
    }
  }
  return null;
}
