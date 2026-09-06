import { describe, expect, it } from "vitest";
import type { FrameworkEdgeOut } from "../api/types";
import { frameworkEdgeGroupsAreEmpty, groupFrameworkEdges } from "./frameworkEdges";

function edge(overrides: Partial<FrameworkEdgeOut>): FrameworkEdgeOut {
  return {
    direction: "src",
    kind: "render_view",
    src_path: "app/controllers/x_controller.rb",
    src_line: 3,
    src_symbol: null,
    dst_kind: null,
    dst_path: "app/views/x/show.html.erb",
    dst_symbol: null,
    trust: "likely",
    extra_json: null,
    ...overrides,
  };
}

describe("groupFrameworkEdges", () => {
  it("splits src-direction edges into produces, dst-direction into targets", () => {
    const edges: FrameworkEdgeOut[] = [
      edge({ direction: "src", kind: "render_view" }),
      edge({
        direction: "dst",
        kind: "route_action",
        src_path: "config/routes.rb",
        src_line: 2,
        dst_path: "app/controllers/x_controller.rb",
        dst_symbol: "x#show",
      }),
    ];
    const groups = groupFrameworkEdges(edges);
    expect(groups.produces).toHaveLength(1);
    expect(groups.targets).toHaveLength(1);
    expect(groups.produces[0].kind).toBe("render_view");
    expect(groups.targets[0].kind).toBe("route_action");
  });

  it("humanizes kind into a spaced label while keeping the raw kind", () => {
    const groups = groupFrameworkEdges([edge({ kind: "render_partial" })]);
    expect(groups.produces[0].kind).toBe("render_partial");
    expect(groups.produces[0].kindLabel).toBe("render partial");
  });

  it("carries trust through verbatim", () => {
    const groups = groupFrameworkEdges([edge({ trust: "candidate" })]);
    expect(groups.produces[0].trust).toBe("candidate");
  });

  it("a src row links to dst_path with no line (rails_edges carries none)", () => {
    const groups = groupFrameworkEdges([
      edge({ direction: "src", dst_path: "app/views/x/show.html.erb", dst_symbol: null }),
    ]);
    expect(groups.produces[0].linkPath).toBe("app/views/x/show.html.erb");
    expect(groups.produces[0].linkLine).toBeNull();
  });

  it("a dst row links to src_path:src_line", () => {
    const groups = groupFrameworkEdges([
      edge({
        direction: "dst",
        src_path: "config/routes.rb",
        src_line: 2,
        dst_path: "app/controllers/x_controller.rb",
      }),
    ]);
    expect(groups.targets[0].linkPath).toBe("config/routes.rb");
    expect(groups.targets[0].linkLine).toBe(2);
  });

  it("falls back to dst_symbol then dst_kind as detail when both are given", () => {
    const withSymbol = groupFrameworkEdges([
      edge({ direction: "src", dst_symbol: "users#create", dst_kind: "controller_action" }),
    ]);
    expect(withSymbol.produces[0].detail).toBe("users#create");

    const kindOnly = groupFrameworkEdges([edge({ direction: "src", dst_symbol: null, dst_kind: "dom_id" })]);
    expect(kindOnly.produces[0].detail).toBe("dom_id");
  });

  it("a dom-id-only turbo_stream target with no dst_path renders no link (plain text)", () => {
    const groups = groupFrameworkEdges([
      edge({
        direction: "src",
        kind: "turbo_stream_target",
        dst_path: null,
        dst_kind: "dom_id",
        dst_symbol: "offers_tab",
      }),
    ]);
    expect(groups.produces[0].linkPath).toBeNull();
    expect(groups.produces[0].detail).toBe("offers_tab");
  });

  it("preserves server row order within each group", () => {
    const edges: FrameworkEdgeOut[] = [
      edge({ direction: "src", kind: "render_view" }),
      edge({ direction: "src", kind: "render_partial" }),
      edge({ direction: "src", kind: "turbo_stream_target" }),
    ];
    const groups = groupFrameworkEdges(edges);
    expect(groups.produces.map((r) => r.kind)).toEqual(["render_view", "render_partial", "turbo_stream_target"]);
  });

  it("assigns stable, unique keys even for two identical-shaped edges", () => {
    const edges: FrameworkEdgeOut[] = [
      edge({ direction: "src", kind: "render_partial" }),
      edge({ direction: "src", kind: "render_partial" }),
    ];
    const groups = groupFrameworkEdges(edges);
    const keys = groups.produces.map((r) => r.key);
    expect(new Set(keys).size).toBe(2);
  });

  it("an empty edge list produces two empty groups", () => {
    const groups = groupFrameworkEdges([]);
    expect(groups.produces).toEqual([]);
    expect(groups.targets).toEqual([]);
  });
});

describe("frameworkEdgeGroupsAreEmpty", () => {
  it("is true when both groups are empty", () => {
    expect(frameworkEdgeGroupsAreEmpty({ produces: [], targets: [] })).toBe(true);
  });

  it("is false when either group has a row", () => {
    const groups = groupFrameworkEdges([edge({ direction: "src" })]);
    expect(frameworkEdgeGroupsAreEmpty(groups)).toBe(false);
    const groups2 = groupFrameworkEdges([edge({ direction: "dst", src_path: "a.rb" })]);
    expect(frameworkEdgeGroupsAreEmpty(groups2)).toBe(false);
  });
});
