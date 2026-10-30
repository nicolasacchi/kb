import { describe, it, expect } from "vitest";
import { renderToString } from "react-dom/server";
import { AsOfBarView } from "./AsOfBar";
import { resolveMemento } from "../../lib/memento";
import type { Version } from "../../api/versions";

const v = (ref: string, source: Version["source"], ts_unix: number): Version => ({
  ref,
  source,
  label: "",
  author: "",
  ts_unix,
  short: ref,
});
const timeline = [v("WORKING", "working", 300), v("index:2", "index", 200), v("index:1", "index", 100)];
const noop = () => {};

describe("AsOfBarView", () => {
  it("captions a nearest-earlier hit with the sibling caveat", () => {
    const html = renderToString(
      <AsOfBarView memento={resolveMemento(timeline, 250)} loading={false} onToday={noop} />,
    );
    expect(html).toContain('data-testid="asof-caption"');
    expect(html).toContain("nearest earlier version");
    expect(html).toContain("relative links resolve to today");
    expect(html).toContain('data-testid="asof-today"');
    expect(html).not.toContain("asof-bar--miss");
  });

  it("a miss is muted and names the floor", () => {
    const html = renderToString(
      <AsOfBarView memento={resolveMemento(timeline, 50)} loading={false} onToday={noop} />,
    );
    expect(html).toContain("asof-bar--miss");
    expect(html).toContain("nothing is that old");
  });

  it("offers then|now only when a split handler is given, and the close form when open", () => {
    const none = renderToString(
      <AsOfBarView memento={resolveMemento(timeline, 250)} loading={false} onToday={noop} />,
    );
    expect(none).not.toContain("asof-split");
    const open = renderToString(
      <AsOfBarView memento={resolveMemento(timeline, 250)} loading={false} split="open" onSplit={noop} onToday={noop} />,
    );
    expect(open).toContain("then | now");
    const close = renderToString(
      <AsOfBarView memento={resolveMemento(timeline, 250)} loading={false} split="close" onSplit={noop} onToday={noop} />,
    );
    expect(close).toContain("close split");
  });

  it("says it is loading before the timeline lands", () => {
    const html = renderToString(<AsOfBarView memento={null} loading onToday={noop} />);
    expect(html).toContain("opening an earlier version");
  });
});
