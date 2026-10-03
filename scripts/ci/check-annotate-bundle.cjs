#!/usr/bin/env node
// Classic-script guard for web/dist/annotate.js (A11.f2, hardened in v0.44 F1).
//
// The daemon injects annotate.js as a CLASSIC <script defer> into the
// artifact's own document, so any top-level function/var/let/const it
// declares lands in the page's global scope and can collide with the
// artifact's own names (the vite 8 mangler did exactly that and the whole
// script died with a SyntaxError). The bundle must therefore be exactly ONE
// top-level statement -- the IIFE -- with no module syntax.
//
// The earlier guard only inspected the START of the bundle, so
// `(function(){})();var x=1;` passed. This one PARSES the file (the
// @babel/parser that web/'s toolchain already installs) as a script and
// inspects every top-level statement.
//
// Usage (run from web/): node ../scripts/ci/check-annotate-bundle.cjs dist/annotate.js
//        node ../scripts/ci/check-annotate-bundle.cjs --self-test
"use strict";
const fs = require("fs");
const path = require("path");

function load() {
  try {
    return require(require.resolve("@babel/parser", { paths: [process.cwd(), __dirname] }));
  } catch (e) {
    console.error("::error::@babel/parser not resolvable (run `npm ci` in web/ first): " + e.message);
    process.exit(2);
  }
}

// Returns a list of problems; empty means the bundle is one IIFE statement.
function check(src, parser) {
  let ast;
  try {
    ast = parser.parse(src, { sourceType: "script", errorRecovery: false });
  } catch (e) {
    return ["does not parse as a classic script (import/export or syntax error): " + e.message];
  }
  const problems = [];
  const stmts = ast.program.body.filter((s) => s.type !== "EmptyStatement");
  if (stmts.length !== 1) {
    problems.push(
      "has " + stmts.length + " top-level statements (want exactly 1, the IIFE): " +
        stmts.map((s) => s.type).join(", ")
    );
  }
  for (const s of stmts) {
    if (s.type !== "ExpressionStatement") {
      problems.push("top-level " + s.type + " would leak into the host page's global scope");
      continue;
    }
    let e = s.expression;
    while (e.type === "UnaryExpression" || e.type === "ParenthesizedExpression" || e.type === "AwaitExpression") e = e.argument || e.expression;
    if (e.type === "SequenceExpression") {
      problems.push("top-level sequence expression; expected a single IIFE call");
      continue;
    }
    if (e.type !== "CallExpression") problems.push("top-level " + e.type + " is not an IIFE call");
    else {
      let callee = e.callee;
      while (callee.type === "ParenthesizedExpression") callee = callee.expression;
      if (callee.type !== "FunctionExpression" && callee.type !== "ArrowFunctionExpression")
        problems.push("top-level call is not an immediately-invoked function (" + callee.type + ")");
    }
  }
  if (/\bimport\.meta\b/.test(src)) problems.push("uses import.meta; it is injected as a classic script");
  return problems;
}

function selfTest(parser) {
  const cases = [
    ["(function(){var a=1;})();", true],
    ["!function(){var a=1;}();", true],
    ["(()=>{var a=1;})();\n//# sourceMappingURL=x.map", true],
    ["(function(){})();var x=1;", false],
    ["(function(){})();function leak(){}", false],
    ["(function(){})();(function(){})();", false],
    ["var x=1;(function(){})();", false],
    ["const y=2;", false],
    ["export {};", false],
    ["import x from 'y';", false],
    ["(function(){})(); window.x = 1;", false],
  ];
  let bad = 0;
  for (const [src, ok] of cases) {
    const got = check(src, parser).length === 0;
    if (got !== ok) {
      console.error("SELFTEST FAIL: " + JSON.stringify(src) + " expected " + (ok ? "accept" : "reject") + ", got " + (got ? "accept" : "reject"));
      bad++;
    }
  }
  if (bad) process.exit(1);
  console.log("check-annotate-bundle self-test: " + cases.length + " cases ok");
}

const parser = load();
const arg = process.argv[2];
if (arg === "--self-test") {
  selfTest(parser);
} else {
  if (!arg) {
    console.error("usage: check-annotate-bundle.cjs <bundle.js> | --self-test");
    process.exit(2);
  }
  const src = fs.readFileSync(path.resolve(arg), "utf8");
  const problems = check(src, parser);
  if (problems.length) {
    for (const p of problems) console.error("::error::annotate.js " + p);
    process.exit(1);
  }
  console.log("annotate.js: a single IIFE statement, no module syntax (" + src.length + " chars)");
}
