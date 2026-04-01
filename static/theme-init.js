// theme-init.js — loaded in <head> to apply saved theme before first paint,
// preventing a flash of the default system theme on page load.
try {
  var _t = localStorage.getItem('rustchan_theme');
  if (_t) {
    document.documentElement.setAttribute('data-theme', _t);
  } else {
    var _default = document.documentElement.getAttribute('data-default-theme') || 'fluorogrid';
    if (_default) document.documentElement.setAttribute('data-theme', _default);
  }
} catch (e) {}
