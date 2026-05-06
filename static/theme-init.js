// Switch the page into JS mode and apply the server-selected theme before first paint.
try {
  document.documentElement.classList.remove('no-js');
  document.documentElement.classList.add('js');
  var _slugs = (document.documentElement.getAttribute('data-theme-slugs') || '')
    .split(',')
    .filter(function (value) { return value; });
  var _valid = function (slug) {
    return _slugs.indexOf(slug) !== -1;
  };
  var _applyThemeCss = function (slug) {
    var existing = document.getElementById('active-theme-stylesheet');
    if (!slug || slug === 'terminal') {
      if (existing) existing.remove();
      return;
    }
    if (!existing) {
      existing = document.createElement('link');
      existing.rel = 'stylesheet';
      existing.id = 'active-theme-stylesheet';
      document.head.appendChild(existing);
    }
    existing.href = '/theme-css/' + encodeURIComponent(slug);
  };
  var _t = document.documentElement.getAttribute('data-active-theme') ||
    document.documentElement.getAttribute('data-theme') ||
    document.documentElement.getAttribute('data-default-theme') ||
    'forest';
  if (_valid(_t)) {
    if (_t === 'terminal') {
      document.documentElement.removeAttribute('data-theme');
    } else {
      document.documentElement.setAttribute('data-theme', _t);
    }
    _applyThemeCss(_t);
    try { localStorage.setItem('rustchan_theme', _t); } catch (e) {}
  }
} catch (e) {}
