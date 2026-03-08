// theme-init.js — loaded in <head> to apply saved theme before first paint,
// preventing a flash of the default terminal theme on page load.
//
// On first visit (no localStorage entry) we fall back to the site-configured
// default theme, which the server injects as data-default-theme on <html>.
// The chosen value is then persisted to localStorage so subsequent pages load
// without re-reading the attribute.
try {
  var _t = localStorage.getItem('rustchan_theme');
  if (!_t) {
    // First visit — check for a server-configured default.
    var _d = document.documentElement.getAttribute('data-default-theme');
    if (_d && _d !== 'terminal') {
      _t = _d;
      localStorage.setItem('rustchan_theme', _t);
    }
  }
  if (_t && _t !== 'terminal') document.documentElement.setAttribute('data-theme', _t);
} catch (e) {}
