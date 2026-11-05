/* InputWeave website: the hero animation, the download section and small
   conveniences. Every page works without this script; it only adds
   motion, picks the visitor's platform and fills in the newest release. */
(function () {
  'use strict';

  var doc = document;
  var root = doc.documentElement;
  var lang = root.lang === 'zh-Hant' ? 'zh-Hant' : 'en';
  var REPO = 'tasict/InputWeave';
  var DOWNLOAD_PREFIX = 'https://github.com/' + REPO + '/releases/download/';

  function each(selector, fn) {
    Array.prototype.forEach.call(doc.querySelectorAll(selector), fn);
  }
  function load(key) {
    try { return window.localStorage.getItem(key); } catch (e) { return null; }
  }
  function save(key, value) {
    try { window.localStorage.setItem(key, value); } catch (e) { /* storage unavailable */ }
  }

  /* Language: the switch is a plain link to the other page. Choosing one
     is remembered, and the English home page follows that choice when the
     visitor arrives from elsewhere. */
  each('[data-lang-switch]', function (a) {
    a.addEventListener('click', function () { save('iw-lang', a.getAttribute('hreflang') || ''); });
  });
  if (doc.body.getAttribute('data-page') === 'home' && lang === 'en' && load('iw-lang') === 'zh-Hant') {
    var fromHere = false;
    try { fromHere = !!doc.referrer && new URL(doc.referrer).origin === window.location.origin; } catch (e) { fromHere = false; }
    if (!fromHere) { window.location.replace('zh-Hant/' + window.location.hash); return; }
  }

  /* Copy buttons: data-copy-from names the element whose text is copied. */
  each('[data-copy-from]', function (btn) {
    var label = btn.textContent;
    btn.addEventListener('click', function () {
      var source = doc.querySelector(btn.getAttribute('data-copy-from'));
      if (!source) return;
      var text = source.textContent.trim();
      var done = function () {
        btn.textContent = btn.getAttribute('data-copied') || label;
        window.setTimeout(function () { btn.textContent = label; }, 1500);
      };
      var selectIt = function () {
        var range = doc.createRange();
        range.selectNodeContents(source);
        var sel = window.getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
      };
      try {
        navigator.clipboard.writeText(text).then(done, selectIt);
      } catch (e) {
        selectIt();
      }
    });
  });

  /* Platform. Phones and tablets get no guess. */
  var ua = navigator.userAgent || '';
  var os = null;
  if (/Android|iPhone|iPad|iPod/i.test(ua)) os = null;
  else if (/Windows/i.test(ua)) os = 'win';
  else if (/Macintosh|Mac OS X/i.test(ua)) os = 'mac';
  else if (/Linux|X11|CrOS/i.test(ua)) os = 'linux';

  var assetURL = { mac: null, win: null };
  function updateHero() {
    var hero = doc.querySelector('[data-hero-download]');
    if (!hero || !os) return;
    var label = hero.getAttribute('data-label-' + os);
    if (label) hero.textContent = label;
    hero.setAttribute('href', assetURL[os] || '#download');
  }
  if (os) {
    each('[data-platform]', function (el) {
      var mine = el.getAttribute('data-platform') === os;
      el.classList.toggle('mine', mine);
      var note = el.querySelector('.mine-note');
      if (note) note.hidden = !mine;
    });
  }
  updateHero();

  /* The newest release, from GitHub's API: one request per page load.
     Until it answers, or if it fails, the page keeps the version and the
     releases page it was published with. */
  function fileNames(v) {
    return {
      mac: 'InputWeave-' + v + '.pkg',
      win: 'InputWeave-' + v + '-x64.msi',
      amd64: 'inputweave_' + v + '_amd64.deb',
      arm64: 'inputweave_' + v + '_arm64.deb'
    };
  }
  function applyRelease(r) {
    var v = String(r.tag_name || '').replace(/^v/, '');
    if (!/^\d+\.\d+\.\d+$/.test(v)) return;
    var names = fileNames(v);
    var found = {};
    (r.assets || []).forEach(function (a) {
      var url = String(a.browser_download_url || '');
      if (url.indexOf(DOWNLOAD_PREFIX) !== 0) return;
      Object.keys(names).forEach(function (k) { if (a.name === names[k]) found[k] = url; });
    });
    each('[data-latest="version"]', function (el) { el.textContent = v; });
    each('[data-file]', function (el) {
      var k = el.getAttribute('data-file');
      if (names[k]) el.textContent = names[k];
    });
    each('[data-asset]', function (el) {
      var k = el.getAttribute('data-asset');
      if (found[k]) el.setAttribute('href', found[k]);
    });
    each('[data-apt]', function (el) { el.textContent = 'sudo apt install ./' + names.amd64; });
    assetURL.mac = found.mac || null;
    assetURL.win = found.win || null;
    updateHero();
    var when = r.published_at ? new Date(r.published_at) : null;
    if (when && !isNaN(when.getTime())) {
      var text;
      try {
        text = new Intl.DateTimeFormat(lang === 'zh-Hant' ? 'zh-TW' : 'en', { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' }).format(when);
      } catch (e) {
        text = r.published_at.slice(0, 10);
      }
      each('[data-latest="date"]', function (el) {
        el.textContent = text;
        el.setAttribute('datetime', r.published_at.slice(0, 10));
      });
      each('[data-latest-dated]', function (el) { el.hidden = false; });
    }
  }
  if (doc.querySelector('[data-latest]') && window.fetch) {
    window.fetch('https://api.github.com/repos/' + REPO + '/releases/latest', { headers: { Accept: 'application/vnd.github+json' } })
      .then(function (res) { return res.ok ? res.json() : null; })
      .then(function (json) { if (json) applyRelease(json); })
      .catch(function () { /* keep what the page was published with */ });
  }

  /* The hero: three screens on the arrangement canvas. A seven-second
     sequence copies a command on the Mac, pastes it in the Ubuntu
     terminal and drags a file from the Windows desktop to the Mac; after
     that, the visitor's own pointer lights the seam it crosses. */
  var svg = doc.getElementById('hero-svg');
  if (!svg) return;
  var reduce = !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
  var $ = function (id) { return doc.getElementById(id); };

  var seams = [
    { x: 370, y1: 160, y2: 300, el: $('glow1'), bell: $('bell1'), anim: null, timer: 0 },
    { x: 730, y1: 100, y2: 300, el: $('glow2'), bell: $('bell2'), anim: null, timer: 0 }
  ];
  var tiles = [
    { x: 110, y: 160, w: 260, h: 170, badge: [222, 304] },
    { x: 370, y: 80, w: 360, h: 220, badge: [530, 262] },
    { x: 730, y: 100, w: 150, h: 270, badge: [785, 300] }
  ];

  /* The edge glow as the apps draw it: the whole seam lights up, brightest
     where the pointer crossed; 0.1 s in, 0.3 s held, 0.4 s out. Crossing
     again while it shows carries on from the current brightness. */
  function glow(s, y) {
    var f = Math.max(0, Math.min(1, (y - s.y1) / (s.y2 - s.y1)));
    var stops = s.bell.querySelectorAll('stop');
    stops[1].setAttribute('offset', Math.max(0, f - 0.32));
    stops[2].setAttribute('offset', f);
    stops[3].setAttribute('offset', Math.min(1, f + 0.32));
    if (reduce || !s.el.animate) {
      s.el.style.opacity = '1';
      window.clearTimeout(s.timer);
      s.timer = window.setTimeout(function () { s.el.style.opacity = '0'; }, 500);
      return;
    }
    var from = parseFloat(window.getComputedStyle(s.el).opacity) || 0;
    if (s.anim) { s.anim.cancel(); s.anim = null; }
    s.el.style.opacity = '0';
    s.anim = s.el.animate(
      [{ opacity: from }, { opacity: 1, offset: 0.125 }, { opacity: 1, offset: 0.5 }, { opacity: 0 }],
      { duration: 800, easing: 'ease-out' }
    );
  }

  var currentTile = -1;
  function tileAt(x, y) {
    for (var i = 0; i < tiles.length; i++) {
      var t = tiles[i];
      if (x >= t.x && x <= t.x + t.w && y >= t.y && y <= t.y + t.h) return i;
    }
    return -1;
  }
  function moveBadge(i) {
    if (i < 0 || i === currentTile) return;
    currentTile = i;
    $('badge').style.transform = 'translate(' + tiles[i].badge[0] + 'px, ' + tiles[i].badge[1] + 'px)';
  }
  var previous = null;
  function pointAt(x, y) {
    if (previous) {
      seams.forEach(function (s) {
        var crossed = (previous[0] < s.x && x >= s.x) || (previous[0] > s.x && x <= s.x);
        if (!crossed) return;
        var dx = x - previous[0] || 1;
        var yAt = previous[1] + (y - previous[1]) * ((s.x - previous[0]) / dx);
        if (yAt >= s.y1 && yAt <= s.y2) glow(s, yAt);
      });
    }
    previous = [x, y];
    moveBadge(tileAt(x, y));
  }

  var cursor = $('cursor');
  var file = $('file');
  var selection = $('sel');
  var turn = $('turn');
  function show(id, on) { $(id).setAttribute('opacity', on ? '1' : '0'); }
  function placeCursor(x, y) { cursor.setAttribute('transform', 'translate(' + x + ' ' + y + ')'); }

  var path = [
    [0, 230, 258], [600, 230, 258], [1000, 182, 240], [1700, 338, 240], [2100, 338, 240],
    [3300, 560, 200], [3400, 560, 200], [4100, 790, 175], [4600, 790, 175], [5400, 452, 145],
    [5700, 452, 145], [6700, 140, 232], [7300, 140, 232]
  ];
  var dragging = false;
  var events = [
    [60, function () { show('cursor', true); }],
    [1900, function () { show('chip-copied', true); }],
    [2900, function () { show('chip-copied', false); }],
    [4300, function () { show('paste1', true); show('paste2', true); show('chip-pasted', true); }],
    [4700, function () { show('paste3', true); show('paste4', true); }],
    [5300, function () { show('chip-pasted', false); }],
    [5700, function () { dragging = true; }],
    [6800, function () { dragging = false; file.setAttribute('transform', 'translate(122 205)'); show('chip-recv', true); }],
    [7400, function () { show('cursor', false); }],
    [7700, function () { finish(); }]
  ];
  var raf = 0;
  var started = 0;
  var fired = 0;
  var running = false;
  var interactive = false;

  function ease(t) { return t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2; }
  function positionAt(t) {
    for (var i = 1; i < path.length; i++) {
      if (t <= path[i][0]) {
        var a = path[i - 1];
        var b = path[i];
        var k = ease(Math.max(0, Math.min(1, (t - a[0]) / ((b[0] - a[0]) || 1))));
        return [a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k];
      }
    }
    var last = path[path.length - 1];
    return [last[1], last[2]];
  }
  function reset() {
    selection.setAttribute('width', '0');
    ['chip-copied', 'chip-pasted', 'chip-recv', 'paste1', 'paste2', 'paste3', 'paste4', 'cursor'].forEach(function (id) { show(id, false); });
    file.setAttribute('transform', 'translate(441 128)');
    currentTile = -1;
    moveBadge(0);
    previous = null;
    dragging = false;
    fired = 0;
    turn.hidden = true;
  }
  function finalState() {
    selection.setAttribute('width', '158');
    ['paste1', 'paste2', 'paste3', 'paste4', 'chip-recv'].forEach(function (id) { show(id, true); });
    ['cursor', 'chip-copied', 'chip-pasted'].forEach(function (id) { show(id, false); });
    file.setAttribute('transform', 'translate(122 205)');
    moveBadge(0);
  }
  function frame(now) {
    var t = now - started;
    while (fired < events.length && t >= events[fired][0]) { events[fired][1](); fired++; }
    if (!running) return;
    var p = positionAt(t);
    placeCursor(p[0], p[1]);
    if (t >= 1000 && t <= 1750) selection.setAttribute('width', String(Math.max(0, Math.min(158, p[0] - 180))));
    if (dragging) file.setAttribute('transform', 'translate(' + (p[0] - 10) + ' ' + (p[1] - 6) + ')');
    pointAt(p[0], p[1]);
    if (t < 7800) raf = window.requestAnimationFrame(frame);
  }
  function play() {
    window.cancelAnimationFrame(raf);
    reset();
    interactive = false;
    running = true;
    started = window.performance.now();
    raf = window.requestAnimationFrame(frame);
  }
  function finish() {
    running = false;
    window.cancelAnimationFrame(raf);
    finalState();
    interactive = true;
    previous = null;
    turn.hidden = false;
    $('replay').hidden = reduce;
  }
  function toDrawing(e) {
    var m = svg.getScreenCTM();
    if (!m) return null;
    var pt = svg.createSVGPoint();
    pt.x = e.clientX;
    pt.y = e.clientY;
    var r = pt.matrixTransform(m.inverse());
    return [r.x, r.y];
  }
  svg.addEventListener('pointermove', function (e) {
    if (running) finish();
    if (!interactive) return;
    var p = toDrawing(e);
    if (p) pointAt(p[0], p[1]);
  });
  svg.addEventListener('pointerleave', function () { previous = null; });
  $('replay').addEventListener('click', function () {
    if (reduce) { finalState(); return; }
    play();
  });

  reset();
  if (reduce) {
    finish();
  } else if ('IntersectionObserver' in window) {
    var seen = new window.IntersectionObserver(function (entries) {
      if (entries.some(function (en) { return en.isIntersecting; })) {
        seen.disconnect();
        window.setTimeout(play, 400);
      }
    }, { threshold: 0.35 });
    seen.observe(svg);
  } else {
    window.setTimeout(play, 500);
  }
})();
