// theme-init.js — loaded in <head> to switch the page into JS mode and apply
// any saved theme preference before first paint.
try {
  document.documentElement.classList.remove('no-js');
  document.documentElement.classList.add('js');
  var _t = localStorage.getItem('rustchan_theme');
  if (_t) {
    document.documentElement.setAttribute('data-theme', _t);
  } else {
    var _default = document.documentElement.getAttribute('data-default-theme') || 'fluorogrid';
    if (_default) document.documentElement.setAttribute('data-theme', _default);
  }
} catch (e) {}
