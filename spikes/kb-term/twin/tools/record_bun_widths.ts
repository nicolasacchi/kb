// bun tools/record_bun_widths.ts glyphs.json OUT.json  (zero deps)
const [gp, out] = Bun.argv.slice(2);
const glyphs = JSON.parse(await Bun.file(gp).text()).glyphs;
const widths: Record<string, { alone: number; ctx: number; alone_ambiguous_wide: number }> = {};
for (const g of glyphs) {
  widths[g.id] = {
    alone: Bun.stringWidth(g.text),
    ctx: Bun.stringWidth("a" + g.text + "b") - 2,
    alone_ambiguous_wide: Bun.stringWidth(g.text, { ambiguousIsNarrow: false }),
  };
}
await Bun.write(out, JSON.stringify({ schema: "kbt-widths/1", tool: "bun " + Bun.version, method: "Bun.stringWidth(glyph); ctx = stringWidth(a+glyph+b)-2", widths }, null, 1));
console.error("bun", Bun.version, glyphs.length, "glyphs");
