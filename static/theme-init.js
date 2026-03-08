// theme-init.js — loaded in <head> to apply saved theme before first paint,
// preventing a flash of the default terminal theme on page load.
try {
  var _t = localStorage.getItem('rustchan_theme');
  if (_t && _t !== 'terminal') document.documentElement.setAttribute('data-theme', _t);
} catch (e) {}
