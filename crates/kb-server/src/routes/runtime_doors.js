// OL IDEA6 - "the citation is the door". Spliced verbatim into the artifact
// runtime (`runtime_js` in artifact.rs). The parent SPA posts
// `{kind: 'kb:coderefs', doors: [{ordinal, raw, declared, href, cls, title}]}`;
// this block marks the exactly-matching <code> elements with ATTRIBUTES and
// inline style properties only (never a wrapped, inserted or removed node, so
// the annotator's css-path and snippet anchors are untouched), and opens a
// door in a new tab from the click itself. Nothing is posted back.
function kbInstallCodeDoors(TARGET) {
  var CLASSES = { exact: 1, moved: 1, unsure: 1, hint: 1 };
  var LINE_STYLE = { exact: 'solid', moved: 'solid', unsure: 'dotted', hint: 'dashed' };
  var MAX_DOORS = 2000;
  var last = null;         // the last validated door list (applied on load)
  var applied = [];        // decoration records, for undecorate
  var byEl = new Map();    // decorated element -> door

  function validate(list) {
    var out = [];
    if (!Array.isArray(list)) return out;
    for (var i = 0; i < list.length && out.length < MAX_DOORS; i++) {
      var d = list[i];
      if (!d || typeof d !== 'object') continue;
      var ord = d.ordinal;
      if (typeof ord !== 'number' || !isFinite(ord) || ord < 0 || Math.floor(ord) !== ord) continue;
      if (typeof d.raw !== 'string' || d.raw.length === 0 || d.raw.length > 2048) continue;
      if (typeof d.href !== 'string') continue;
      var u;
      try { u = new URL(d.href); } catch (e) { continue; }
      if (u.protocol !== 'http:' && u.protocol !== 'https:') continue;
      var cls = (typeof d.cls === 'string' && CLASSES[d.cls] === 1) ? d.cls : 'hint';
      var title = d.title == null ? '' : String(d.title);
      if (title.length > 300) title = title.slice(0, 300);
      out.push({ ordinal: ord, raw: d.raw, declared: d.declared === true, href: u.href, cls: cls, title: title });
    }
    out.sort(function (a, b) { return a.ordinal - b.ordinal; });
    return out;
  }

  function undecorate() {
    for (var i = applied.length - 1; i >= 0; i--) {
      var r = applied[i];
      var el = r.el;
      var names = Object.keys(r.attrs);
      for (var j = 0; j < names.length; j++) {
        var prior = r.attrs[names[j]];
        if (prior === null) el.removeAttribute(names[j]); else el.setAttribute(names[j], prior);
      }
      if (el.getAttribute('style') === r.styleAfter) {
        if (r.stylePrior === null) el.removeAttribute('style'); else el.setAttribute('style', r.stylePrior);
      } else {
        var props = Object.keys(r.props);
        for (var k = 0; k < props.length; k++) {
          var p = r.props[props[k]];
          if (p.value) el.style.setProperty(props[k], p.value, p.priority); else el.style.removeProperty(props[k]);
        }
        if (r.stylePrior === null && el.getAttribute('style') === '') el.removeAttribute('style');
      }
    }
    applied = [];
    byEl = new Map();
  }

  function inert(el) {
    return !!(el.closest && el.closest('a[href],button,summary,label,[contenteditable]'));
  }

  function decorate(el, door) {
    var rec = { el: el, attrs: {}, props: {}, stylePrior: el.getAttribute('style'), styleAfter: null };
    var set = {
      'data-kb-coderef': String(door.ordinal),
      'data-kb-coderef-cls': door.cls,
      'title': door.title,
      'role': 'link',
      'tabindex': '0'
    };
    var names = Object.keys(set);
    for (var i = 0; i < names.length; i++) {
      rec.attrs[names[i]] = el.getAttribute(names[i]);
      el.setAttribute(names[i], set[names[i]]);
    }
    var css = {
      'text-decoration-line': 'underline',
      'text-decoration-style': LINE_STYLE[door.cls],
      'text-underline-offset': '0.2em',
      'cursor': 'pointer'
    };
    var props = Object.keys(css);
    for (var j = 0; j < props.length; j++) {
      rec.props[props[j]] = { value: el.style.getPropertyValue(props[j]), priority: el.style.getPropertyPriority(props[j]) };
      el.style.setProperty(props[j], css[props[j]]);
    }
    rec.styleAfter = el.getAttribute('style');
    applied.push(rec);
    byEl.set(el, door);
  }

  function apply() {
    undecorate();
    if (!last || !document.body || document.readyState === 'loading') return;
    var queues = Object.create(null);
    for (var i = 0; i < last.length; i++) {
      var d = last[i];
      var key = (d.declared ? 'd:' : 't:') + d.raw;
      (queues[key] || (queues[key] = [])).push(d);
    }
    var codes = document.body.getElementsByTagName('code');
    for (var n = 0; n < codes.length; n++) {
      var el = codes[n];
      if (el.parentElement && el.parentElement.closest('code')) continue;   // the outer element owns it
      var k;
      if (el.hasAttribute('data-kb-ref')) k = 'd:' + el.getAttribute('data-kb-ref');
      else {
        var t = (el.textContent || '').trim();
        if (!t) continue;
        k = 't:' + t;
      }
      var q = queues[k];
      if (!q || q.length === 0) continue;
      var door = q.shift();                  // the lowest remaining ordinal
      if (inert(el)) continue;               // consumed, not decorated
      decorate(el, door);
    }
  }

  function selectionTouches(el) {
    try {
      var sel = window.getSelection && window.getSelection();
      if (!sel || sel.isCollapsed || sel.rangeCount === 0) return false;
      var rng = sel.getRangeAt(0);
      return rng.intersectsNode ? rng.intersectsNode(el) : el.contains(sel.anchorNode);
    } catch (e) { return false; }
  }

  function doorFor(target) {
    var el = target && target.closest && target.closest('[data-kb-coderef]');
    if (!el || !byEl.has(el)) return null;
    var inner = target.closest('a[href],button,summary,label,[contenteditable]');
    if (inner && el.contains(inner)) return null;   // the author's control inside wins
    return byEl.get(el);
  }

  function open(door) {
    try { window.open(door.href, '_blank', 'noopener,noreferrer'); } catch (e) {}
  }

  window.addEventListener('message', function (ev) {
    if (!ev || ev.source !== window.parent) return;
    if (TARGET !== '*' && ev.origin !== TARGET) return;
    var data = ev.data;
    if (!data || data.kind !== 'kb:coderefs') return;
    last = validate(data.doors);
    if (document.readyState === 'loading') return;   // applied on DOMContentLoaded
    apply();
  });
  document.addEventListener('DOMContentLoaded', function () { if (last) apply(); });

  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0) return;
    var door = doorFor(e.target);
    if (!door) return;
    var el = e.target.closest('[data-kb-coderef]');
    if (selectionTouches(el)) return;
    e.preventDefault();
    open(door);
  });
  document.addEventListener('keydown', function (e) {
    if (e.defaultPrevented || e.key !== 'Enter') return;
    if (e.ctrlKey || e.metaKey || e.altKey || e.shiftKey || e.isComposing) return;
    var door = doorFor(e.target);
    if (!door) return;
    e.preventDefault();
    open(door);
  });
}
