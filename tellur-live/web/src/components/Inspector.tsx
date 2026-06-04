import { FileCode, Flag } from "lucide-react";
import type { NodeKind } from "../types";

// A timeline Event the selected node fires: an absolute time and optional name.
// Mirrors the trigger shape carried on the arrangement (and on a Timeline clip).
export interface SelectedNodeEvent {
  time: number;
  name: string | null;
}

// The data the Inspector renders for the currently-selected timeline node. This
// is lifted up to App so the Inspector (a sibling of the timeline) can read it;
// Timeline builds it from the clicked clip. It is intentionally a small, stable
// projection of the node's `Arrangement` (not the full tree) — the backend may
// add more fields later, at which point this and the clip can carry them too.
export interface SelectedNode {
  // Stable dotted DFS path of the node (matches the clip id), used to round-trip
  // the selection highlight back down to the timeline via `selectedId`.
  id: string;
  name: string;
  kind: NodeKind;
  source: { file: string; line: number } | null;
  start: number;
  end: number;
  triggers: SelectedNodeEvent[];
}

interface InspectorProps {
  node: SelectedNode | null;
}

// Last path segment of a source file path (handles both `/` and `\` separators).
function basename(file: string): string {
  const segments = file.split(/[\\/]/);
  return segments[segments.length - 1] || file;
}

// Plain seconds readout (e.g. "1.50s"). The inspector shows raw seconds rather
// than a timecode so it stays fps-independent and readable at a glance.
function formatSeconds(seconds: number): string {
  return `${seconds.toFixed(2)}s`;
}

// Detail/inspector panel for the selected timeline node. The source location is
// just one field here; this panel is general and can grow as the backend exposes
// more. When nothing is selected it shows a muted placeholder.
export function Inspector({ node }: InspectorProps) {
  return (
    <section className="inspector" aria-label="Inspector">
      <h2 className="inspector__heading">詳細</h2>
      {node ? (
        <div className="inspector__body">
          <div className="inspector__title" title={node.name}>
            {node.name}
          </div>
          <dl className="inspector__fields">
            <div className="inspector__row">
              <dt className="inspector__label">Kind</dt>
              <dd className="inspector__value">{node.kind}</dd>
            </div>
            {node.source ? (
              <div className="inspector__row">
                <dt className="inspector__label">Source</dt>
                <dd className="inspector__value inspector__value--source">
                  <FileCode size={12} strokeWidth={2} />
                  <span className="inspector__source-loc">
                    {basename(node.source.file)}:{node.source.line}
                  </span>
                </dd>
              </div>
            ) : null}
            <div className="inspector__row">
              <dt className="inspector__label">Time</dt>
              <dd className="inspector__value inspector__value--time">
                {formatSeconds(node.start)} – {formatSeconds(node.end)}
              </dd>
            </div>
          </dl>
          {node.triggers.length > 0 ? (
            <div className="inspector__events">
              <div className="inspector__label">Events</div>
              <ul className="inspector__event-list">
                {node.triggers.map((trigger, i) => (
                  <li
                    key={`${trigger.time}:${i}`}
                    className="inspector__event"
                  >
                    <Flag size={11} strokeWidth={2} />
                    <span className="inspector__event-name">
                      {trigger.name ?? "Event"}
                    </span>
                    <span className="inspector__event-time">
                      {formatSeconds(trigger.time)}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </div>
      ) : (
        <p className="inspector__placeholder">ノードを選択してください</p>
      )}
    </section>
  );
}
