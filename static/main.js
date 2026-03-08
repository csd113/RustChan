// main.js — RustChan client-side logic
// FIX[NEW-H1]: All JavaScript has been moved from inline <script> tags to this
// external file, removing the need for 'unsafe-inline' in the CSP script-src
// directive. Dynamic per-page values are passed via data-* attributes on HTML
// elements and read here at runtime.

'use strict';

// ─── Post form toggle & mobile drawer ────────────────────────────────────────

function togglePostForm() {
  var wrap = document.getElementById('post-form-wrap');
  var btn = document.querySelector('.post-toggle-btn');
  if (!wrap) return;
  var opening = wrap.style.display === 'none';
  wrap.style.display = opening ? 'block' : 'none';
  if (btn) btn.classList.toggle('active', opening);
  if (opening) {
    var first = wrap.querySelector('input[type="text"], textarea');
    if (first) first.focus();
  }
}

function toggleMobileDrawer() {
  var drawer = document.getElementById('mobile-reply-drawer');
  var fab = document.getElementById('mobile-reply-fab');
  if (!drawer) return;
  var opening = !drawer.classList.contains('open');
  drawer.classList.toggle('open', opening);
  if (fab) fab.classList.toggle('hidden', opening);
  if (opening) {
    var ta = drawer.querySelector('textarea');
    if (ta) { setTimeout(function () { ta.focus(); }, 120); }
  }
}

function appendReply(id) {
  var wrap = document.getElementById('post-form-wrap');
  var isMobile = window.matchMedia('(max-width: 767px)').matches;
  if (isMobile) {
    var drawer = document.getElementById('mobile-reply-drawer');
    if (drawer && !drawer.classList.contains('open')) toggleMobileDrawer();
    var ta = drawer ? drawer.querySelector('textarea') : null;
    if (ta) { ta.value += '>>' + id + '\n'; ta.focus(); }
  } else {
    if (wrap && wrap.style.display === 'none') togglePostForm();
    var ta2 = document.getElementById('reply-body');
    if (ta2) { ta2.value += '>>' + id + '\n'; ta2.focus(); }
  }
  return false;
}

// ─── Media expand / collapse ─────────────────────────────────────────────────

function expandMedia(preview) {
  var container = preview.closest('.file-container');
  var expanded = container.querySelector('.media-expanded');
  var closeBtn = container.querySelector('.media-close-btn');
  if (expanded.tagName === 'IMG' && expanded.dataset.src) {
    expanded.src = expanded.dataset.src;
    delete expanded.dataset.src;
  }
  preview.style.display = 'none';
  expanded.style.display = 'block';
  closeBtn.style.display = 'inline-flex';
  // Stop floating so expanded media stacks above post text instead of
  // widening the float and shoving text off to the right.
  container.classList.add('media-is-expanded');
  if (expanded.tagName === 'VIDEO') {
    expanded.play().catch(function () {});
  }
  // Wire click-on-expanded to collapse back to thumbnail (once per element).
  if (!expanded.dataset.collapseWired) {
    expanded.dataset.collapseWired = '1';
    if (expanded.tagName === 'IMG') {
      // Clicking the full-size image collapses it.
      expanded.style.cursor = 'zoom-out';
      expanded.addEventListener('click', function () {
        var btn = expanded.closest('.file-container').querySelector('.media-close-btn');
        if (btn) collapseMedia(btn);
      });
    } else if (expanded.tagName === 'VIDEO') {
      // Clicking the video *outside* the native controls bar collapses it.
      // The controls bar is roughly the bottom 40px of the element.
      expanded.addEventListener('click', function (e) {
        var rect = expanded.getBoundingClientRect();
        var controlsHeight = 40;
        if (e.clientY < rect.bottom - controlsHeight) {
          var btn = expanded.closest('.file-container').querySelector('.media-close-btn');
          if (btn) collapseMedia(btn);
        }
      });
    }
  }
}

function collapseMedia(btn) {
  var container = btn.closest('.file-container');
  var expanded = container.querySelector('.media-expanded');
  var preview = container.querySelector('.media-preview');
  if (expanded.tagName === 'VIDEO') {
    expanded.pause();
    expanded.currentTime = 0;
  }
  expanded.style.display = 'none';
  expanded.style.maxWidth = '';
  expanded.style.maxHeight = '';
  // Restore float so thumbnail sits beside post text again.
  container.classList.remove('media-is-expanded');
  preview.style.display = 'block';
  btn.style.display = 'none';
}

function expandVideoEmbed(preview, type, id, container) {
  var iframe = document.createElement('iframe');
  if (type === 'youtube') {
    iframe.src = 'https://www.youtube-nocookie.com/embed/' + id + '?autoplay=1&rel=0';
  } else if (type === 'streamable') {
    iframe.src = 'https://streamable.com/e/' + id + '?autoplay=1';
  }
  iframe.className = 'embed-iframe';
  iframe.setAttribute('frameborder', '0');
  iframe.setAttribute('allowfullscreen', '');
  iframe.setAttribute('allow', 'accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share; fullscreen');
  iframe.setAttribute('referrerpolicy', 'strict-origin-when-cross-origin');
  preview.style.display = 'none';
  var closeBtn = container.querySelector('.media-close-btn');
  if (closeBtn) closeBtn.style.display = 'inline-flex';
  container.appendChild(iframe);
}

function collapseVideoEmbed(btn) {
  var container = btn.closest('.video-embed-container');
  if (!container) return;
  var iframe = container.querySelector('.embed-iframe');
  var preview = container.querySelector('.media-preview');
  if (iframe) { iframe.src = ''; iframe.remove(); }
  if (preview) preview.style.display = '';
  btn.style.display = 'none';
}

// ─── Auto-compress modal ─────────────────────────────────────────────────────
// Dynamic limits (MAX_IMAGE / MAX_VIDEO) are read from data-max-image /
// data-max-video attributes on the #compress-modal element, injected by the
// Rust template at render time.

(function () {
  var _input = null, _file = null, _max = 0, _compressing = false;

  function getMax(type) {
    var modal = document.getElementById('compress-modal');
    if (!modal) return 0;
    if (type === 'image') return parseInt(modal.dataset.maxImage, 10) || 0;
    if (type === 'video') return parseInt(modal.dataset.maxVideo, 10) || 0;
    return 0;
  }

  window.checkFileSize = function (input) {
    var file = input.files && input.files[0];
    if (!file) return;
    var isImg = file.type.startsWith('image/');
    var isVideo = file.type.startsWith('video/');
    var limit = isImg ? getMax('image') : (isVideo ? getMax('video') : 0);
    if (limit === 0 || file.size <= limit) return;
    _input = input;
    _file = file;
    _max = limit;
    var sizeMiB = (file.size / 1048576).toFixed(1);
    var limMiB = (limit / 1048576).toFixed(1);
    var info = document.getElementById('compress-info');
    if (info) info.textContent = '\u201c' + file.name + '\u201d is ' + sizeMiB + ' MiB \u2014 board limit is ' + limMiB + ' MiB.';
    _setView('actions');
    var modal = document.getElementById('compress-modal');
    if (modal) modal.style.display = 'flex';
  };

  window.dismissCompressModal = function () {
    if (_compressing) return;
    var modal = document.getElementById('compress-modal');
    if (modal) modal.style.display = 'none';
    if (_input) { _input.value = ''; }
    _input = null; _file = null; _compressing = false;
  };

  window.startCompress = function () {
    if (!_file || !_input || _compressing) return;
    _compressing = true;
    _setView('progress');
    _setProgress(0, 'Starting\u2026');

    var isImg = _file.type.startsWith('image/');
    var isVideo = _file.type.startsWith('video/');
    var promise = isImg ? _compressImage(_file, _max)
      : isVideo ? _compressVideo(_file, _max)
        : Promise.reject(new Error('Unsupported type'));

    promise.then(function (blob) {
      if (!blob || blob.size > _max) {
        _setProgress(100, 'Could not compress to the required size. Please use a smaller file.');
        _compressing = false;
        _setView('done');
        return;
      }
      var ext = isImg ? 'jpg' : 'webm';
      var newName = _file.name.replace(/\.[^.]+$/, '') + '_compressed.' + ext;
      var dt = new DataTransfer();
      dt.items.add(new File([blob], newName, { type: blob.type }));
      _input.files = dt.files;
      var finalMiB = (blob.size / 1048576).toFixed(2);
      _setProgress(100, '\u2713 Compressed to ' + finalMiB + ' MiB. Ready to post.');
      _compressing = false;
      setTimeout(function () {
        var modal = document.getElementById('compress-modal');
        if (modal) modal.style.display = 'none';
        _input = null; _file = null;
      }, 1200);
    }).catch(function (err) {
      _setProgress(0, 'Error: ' + (err.message || err));
      _compressing = false;
      _setView('done');
    });
  };

  function _setView(which) {
    var acts = document.getElementById('compress-actions');
    var prog = document.getElementById('compress-progress');
    var done = document.getElementById('compress-done-actions');
    if (acts) acts.style.display = which === 'actions' ? 'flex' : 'none';
    if (prog) prog.style.display = which === 'progress' ? 'block' : 'none';
    if (done) done.style.display = which === 'done' ? 'flex' : 'none';
  }

  function _setProgress(pct, text) {
    var bar = document.getElementById('compress-progress-bar');
    var txt = document.getElementById('compress-progress-text');
    if (bar) bar.style.width = pct + '%';
    if (txt) txt.textContent = text;
  }

  function _compressImage(file, maxBytes) {
    return new Promise(function (resolve, reject) {
      var img = new Image();
      var url = URL.createObjectURL(file);
      img.onload = function () {
        URL.revokeObjectURL(url);
        var w = img.naturalWidth, h = img.naturalHeight;
        var scale = 1.0, quality = 0.85;
        var canvas = document.createElement('canvas');
        var ctx = canvas.getContext('2d');
        var attempt = 0;
        function tryEncode() {
          attempt++;
          canvas.width = Math.round(w * scale);
          canvas.height = Math.round(h * scale);
          ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
          canvas.toBlob(function (blob) {
            _setProgress(Math.min(attempt * 15, 90), 'Compressing\u2026 attempt ' + attempt);
            if (!blob) { reject(new Error('Canvas toBlob failed')); return; }
            if (blob.size <= maxBytes) { resolve(blob); return; }
            if (attempt >= 8) { resolve(blob); return; }
            quality -= 0.1;
            if (quality < 0.3) { quality = 0.5; scale *= 0.75; }
            tryEncode();
          }, 'image/jpeg', quality);
        }
        tryEncode();
      };
      img.onerror = function () { URL.revokeObjectURL(url); reject(new Error('Image load failed')); };
      img.src = url;
    });
  }

  function _compressVideo(file, maxBytes) {
    return new Promise(function (resolve, reject) {
      if (!window.MediaRecorder) { reject(new Error('MediaRecorder not supported')); return; }
      var url = URL.createObjectURL(file);
      var videoEl = document.createElement('video');
      videoEl.muted = true;
      videoEl.src = url;
      var duration = 0;
      videoEl.onloadedmetadata = function () {
        duration = videoEl.duration;
        if (!duration || !isFinite(duration)) { URL.revokeObjectURL(url); reject(new Error('Cannot determine video duration')); return; }
        _setProgress(10, 'Analysing video\u2026');
        var targetBitsPerSec = Math.floor((maxBytes * 8) / duration * 0.9);
        var mimeType = MediaRecorder.isTypeSupported('video/webm;codecs=vp9') ? 'video/webm;codecs=vp9' : 'video/webm';
        var stream = null;
        try { stream = videoEl.captureStream ? videoEl.captureStream() : videoEl.mozCaptureStream(); } catch (e) { URL.revokeObjectURL(url); reject(e); return; }
        var recorder = new MediaRecorder(stream, { mimeType: mimeType, videoBitsPerSecond: targetBitsPerSec });
        var chunks = [];
        recorder.ondataavailable = function (e) { if (e.data && e.data.size > 0) chunks.push(e.data); };
        recorder.onstop = function () {
          URL.revokeObjectURL(url);
          resolve(new Blob(chunks, { type: 'video/webm' }));
        };
        recorder.onerror = function (e) { URL.revokeObjectURL(url); reject(e.error || new Error('MediaRecorder error')); };
        videoEl.currentTime = 0;
        videoEl.play().catch(function () {});
        recorder.start();
        var progressTimer = setInterval(function () {
          _setProgress(Math.min(10 + Math.round((videoEl.currentTime / duration) * 80), 90), 'Re-encoding\u2026 ' + Math.round((videoEl.currentTime / duration) * 100) + '%');
        }, 500);
        videoEl.addEventListener('timeupdate', function captureFrame() {
          if (videoEl.currentTime >= duration - 0.1) {
            clearInterval(progressTimer);
            recorder.stop();
            videoEl.removeEventListener('timeupdate', captureFrame);
          }
        });
      };
      videoEl.onerror = function () { URL.revokeObjectURL(url); reject(new Error('Video load error')); };
      videoEl.load();
    });
  }
})();

// ─── Report modal ─────────────────────────────────────────────────────────────

function openReportModal(postId, threadId, board, csrf) {
  document.getElementById('report-post-id').value = postId;
  document.getElementById('report-thread-id').value = threadId;
  document.getElementById('report-board').value = board;
  document.getElementById('report-csrf').value = csrf;
  var info = document.getElementById('report-info');
  if (info) info.textContent = 'Reporting post No.' + postId;
  var reason = document.getElementById('report-reason');
  if (reason) reason.value = '';
  var modal = document.getElementById('report-modal');
  if (modal) modal.style.display = 'flex';
  if (reason) reason.focus();
}

function closeReportModal() {
  var modal = document.getElementById('report-modal');
  if (modal) modal.style.display = 'none';
}

// ─── Theme picker ─────────────────────────────────────────────────────────────

(function () {
  var THEMES = ['terminal', 'aero', 'dorfic', 'fluorogrid', 'neoncubicle'];

  function applyTheme(t) {
    if (t === 'terminal') {
      document.documentElement.removeAttribute('data-theme');
    } else {
      document.documentElement.setAttribute('data-theme', t);
    }
    document.querySelectorAll('.tp-option').forEach(function (el, i) {
      el.classList.toggle('active', THEMES[i] === t);
    });
  }

  window.setTheme = function (t) {
    try { localStorage.setItem('rustchan_theme', t); } catch (e) {}
    applyTheme(t);
    closeThemePicker();
  };

  window.toggleThemePicker = function () {
    var p = document.getElementById('theme-picker-panel');
    if (p) p.classList.toggle('open');
  };

  function closeThemePicker() {
    var p = document.getElementById('theme-picker-panel');
    if (p) p.classList.remove('open');
  }

  document.addEventListener('click', function (e) {
    var btn = document.getElementById('theme-picker-btn');
    var panel = document.getElementById('theme-picker-panel');
    if (btn && panel && !btn.contains(e.target) && !panel.contains(e.target)) {
      closeThemePicker();
    }
  });

  try {
    var saved = localStorage.getItem('rustchan_theme');
    if (saved && THEMES.indexOf(saved) !== -1) { applyTheme(saved); }
  } catch (e) {}
})();

// ─── Collapse greentext blocks ────────────────────────────────────────────────

(function () {
  if (document.body && document.body.getAttribute('data-collapse-greentext') === '1') {
    document.querySelectorAll('details.greentext-block').forEach(function (el) {
      el.removeAttribute('open');
    });
  }
})();

// ─── Thread auto-update ───────────────────────────────────────────────────────

(function () {
  var container = document.getElementById('thread-posts');
  var statusEl = document.getElementById('autoupdate-status');
  var timer = null;
  var updating = false;
  var autoOn = false;

  if (!container) return;

  var board = container.dataset.board;
  var threadId = container.dataset.threadId;
  var lastId = parseInt(container.dataset.lastId, 10) || 0;

  // Floating new-replies pill
  var pill = document.createElement('div');
  pill.id = 'new-replies-pill';
  pill.className = 'new-replies-pill';
  pill.style.display = 'none';
  document.body.appendChild(pill);

  var pillTimer = null;
  var pillCount = 0;

  function showPill(n) {
    pillCount += n;
    pill.textContent = '+' + pillCount + ' new repl' + (pillCount === 1 ? 'y' : 'ies') + ' \u2193';
    pill.style.display = 'block';
    if (pillTimer) clearTimeout(pillTimer);
    pillTimer = setTimeout(hidePill, 30000);
  }

  function hidePill() {
    pill.style.display = 'none';
    pillCount = 0;
    if (pillTimer) { clearTimeout(pillTimer); pillTimer = null; }
  }

  pill.addEventListener('click', function () {
    window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' });
    hidePill();
  });

  window.addEventListener('scroll', function () {
    if (!pillCount) return;
    var distFromBottom = document.body.scrollHeight - window.scrollY - window.innerHeight;
    if (distFromBottom < 200) hidePill();
  }, { passive: true });

  function setStatus(msg) {
    if (statusEl) statusEl.textContent = msg;
  }

  function applyDeltaState(data) {
    var rcEl = document.getElementById('thread-reply-count');
    if (rcEl && data.reply_count !== undefined) rcEl.textContent = data.reply_count;
    var lockedEl = document.getElementById('thread-locked-indicator');
    if (lockedEl && data.locked !== undefined) lockedEl.style.display = data.locked ? '' : 'none';
    var stickyEl = document.getElementById('thread-sticky-indicator');
    if (stickyEl && data.sticky !== undefined) stickyEl.style.display = data.sticky ? '' : 'none';
  }

  window.fetchUpdates = function () {
    if (updating) return;
    updating = true;
    setStatus('updating\u2026');
    fetch('/' + board + '/thread/' + threadId + '/updates?since=' + lastId)
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (data) {
        applyDeltaState(data);
        if (data.count > 0) {
          var frag = document.createElement('div');
          frag.innerHTML = data.html;
          while (frag.firstChild) container.appendChild(frag.firstChild);
          lastId = data.last_id;
          if (window._onNewPostsInserted) window._onNewPostsInserted(container);
          showPill(data.count);
        }
        setStatus('updated');
        setTimeout(function () { setStatus(''); }, 2000);
        updating = false;
      })
      .catch(function () {
        setStatus('update failed');
        updating = false;
      });
  };

  function toggleAutoUpdate(cb) {
    autoOn = cb.checked;
    if (autoOn) {
      if (timer) clearInterval(timer);
      timer = setInterval(window.fetchUpdates, 15000);
      setStatus('auto-update on');
    } else {
      if (timer) { clearInterval(timer); timer = null; }
      setStatus('');
    }
  }

  // Expose for the change handler on the checkbox
  window._toggleAutoUpdate = toggleAutoUpdate;
})();

// ─── "(You)" post tracking ────────────────────────────────────────────────────

(function () {
  var container = document.getElementById('thread-posts');
  if (!container) return;

  var board = container.dataset.board;
  var threadId = container.dataset.threadId;

  var POSTS_KEY = 'rustchan_my_posts_' + board + '_' + threadId;
  var PENDING_KEY = 'rustchan_you_pending_' + board + '_' + threadId;

  try {
    var pending = localStorage.getItem(PENDING_KEY);
    if (pending === '1') {
      localStorage.removeItem(PENDING_KEY);
      var hash = window.location.hash;
      var m = hash.match(/^#p(\d+)$/);
      if (m) {
        var newId = parseInt(m[1], 10);
        var existing = JSON.parse(localStorage.getItem(POSTS_KEY) || '[]');
        if (existing.indexOf(newId) === -1) existing.push(newId);
        localStorage.setItem(POSTS_KEY, JSON.stringify(existing));
      }
    }
  } catch (e) {}

  window._applyYouBadges = function () {
    try {
      var myPosts = JSON.parse(localStorage.getItem(POSTS_KEY) || '[]');
      myPosts.forEach(function (pid) {
        var postEl = document.getElementById('p' + pid);
        if (!postEl) return;
        var postNum = postEl.querySelector('.post-num');
        if (postNum && !postNum.parentNode.querySelector('.you-badge')) {
          var badge = document.createElement('span');
          badge.className = 'you-badge';
          badge.title = 'You posted this';
          badge.textContent = '(You)';
          postNum.insertAdjacentElement('afterend', badge);
        }
      });
    } catch (e) {}
  };

  _applyYouBadges();

  var origInsert = window._onNewPostsInserted;
  window._onNewPostsInserted = function (c) {
    if (origInsert) origInsert(c);
    _applyYouBadges();
  };

  function wireFormTracking() {
    var forms = document.querySelectorAll('form[action*="/thread/' + threadId + '"]');
    forms.forEach(function (form) {
      if (form.dataset.youWired) return;
      form.dataset.youWired = '1';
      form.addEventListener('submit', function () {
        try { localStorage.setItem(PENDING_KEY, '1'); } catch (e) {}
      });
    });
  }
  wireFormTracking();

  document.addEventListener('click', function (e) {
    if (e.target && (e.target.id === 'mobile-reply-fab' || e.target.classList.contains('post-toggle-btn'))) {
      setTimeout(wireFormTracking, 150);
    }
  });
})();

// ─── Quotelink hover preview ──────────────────────────────────────────────────

(function () {
  var _highlighted = null;

  function highlightPost(id) {
    clearHighlight();
    var el = document.getElementById('p' + id);
    if (!el) return;
    el.classList.add('post-highlighted');
    _highlighted = el;
  }

  function clearHighlight() {
    if (_highlighted) {
      _highlighted.classList.remove('post-highlighted');
      _highlighted = null;
    }
  }

  document.addEventListener('click', function (e) {
    if (e.target.classList.contains('quotelink')) return;
    if (e.target.classList.contains('backref')) return;
    clearHighlight();
  });

  var popup = document.createElement('div');
  popup.id = 'ql-popup';
  popup.className = 'quotelink-popup';
  popup.style.display = 'none';
  document.body.appendChild(popup);

  var _popupTarget = null;
  var _hideTimer = null;

  function showPopup(link, pid) {
    var src = document.getElementById('p' + pid);
    if (!src) return;
    var clone = src.cloneNode(true);
    clone.removeAttribute('id');
    clone.querySelectorAll('.post-controls, .admin-post-controls, .post-toggle-bar').forEach(function (n) { n.remove(); });
    popup.innerHTML = '';
    popup.appendChild(clone);
    popup.style.display = 'block';
    _popupTarget = pid;
    positionPopup(link);
  }

  function positionPopup(anchor) {
    var rect = anchor.getBoundingClientRect();
    var pw = popup.offsetWidth || 420;
    var ph = popup.offsetHeight || 200;
    var vw = window.innerWidth;
    var vh = window.innerHeight;
    var scrollY = window.pageYOffset;
    var left = rect.left + window.pageXOffset;
    if (left + pw > vw - 10) left = Math.max(4, vw - pw - 10);
    var top;
    if (rect.bottom + ph + 8 < vh) {
      top = rect.bottom + scrollY + 8;
    } else {
      top = rect.top + scrollY - ph - 8;
    }
    popup.style.left = left + 'px';
    popup.style.top = top + 'px';
  }

  function hidePopup() {
    popup.style.display = 'none';
    _popupTarget = null;
  }

  function wireQuotelinks(root) {
    root.querySelectorAll('a.quotelink[data-pid]').forEach(function (link) {
      var pid = link.getAttribute('data-pid');
      link.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); showPopup(link, pid); });
      link.addEventListener('mouseleave', function () { _hideTimer = setTimeout(hidePopup, 120); });
      link.addEventListener('click', function (e) {
        var target = document.getElementById('p' + pid);
        if (!target) return;
        e.preventDefault();
        var offset = target.getBoundingClientRect().top + window.pageYOffset - 60;
        window.scrollTo({ top: offset, behavior: 'smooth' });
        highlightPost(pid);
        hidePopup();
      });
    });
  }

  popup.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); });
  popup.addEventListener('mouseleave', function () { _hideTimer = setTimeout(hidePopup, 120); });

  function wireBackrefs(root) {
    root.querySelectorAll('a.backref[data-pid]').forEach(function (link) {
      var pid = link.getAttribute('data-pid');
      link.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); showPopup(link, pid); });
      link.addEventListener('mouseleave', function () { _hideTimer = setTimeout(hidePopup, 120); });
      link.addEventListener('click', function (e) {
        var target = document.getElementById('p' + pid);
        if (!target) return;
        e.preventDefault();
        var offset = target.getBoundingClientRect().top + window.pageYOffset - 60;
        window.scrollTo({ top: offset, behavior: 'smooth' });
        highlightPost(pid);
        hidePopup();
      });
    });
  }

  function buildBackrefs() {
    var refs = {};
    document.querySelectorAll('#thread-posts a.quotelink[data-pid]').forEach(function (link) {
      var citedPid = link.getAttribute('data-pid');
      var postEl = link.closest('.post');
      if (!postEl) return;
      var citingId = postEl.id.replace('p', '');
      if (!refs[citedPid]) refs[citedPid] = [];
      if (refs[citedPid].indexOf(citingId) === -1) refs[citedPid].push(citingId);
    });
    Object.keys(refs).forEach(function (citedPid) {
      var span = document.getElementById('backrefs-' + citedPid);
      if (!span) return;
      refs[citedPid].forEach(function (citingId) {
        var a = document.createElement('a');
        a.href = '#p' + citingId;
        a.className = 'backref';
        a.setAttribute('data-pid', citingId);
        a.textContent = '>>' + citingId;
        span.appendChild(a);
      });
      wireBackrefs(span);
    });
  }

  wireQuotelinks(document);
  buildBackrefs();

  if (window._qlHooked) return;
  window._qlHooked = true;
  var _origInsert = window._onNewPostsInserted;
  window._onNewPostsInserted = function (container) {
    if (_origInsert) _origInsert(container);
    wireQuotelinks(container);
    buildBackrefs();
  };
})();

// ─── Cross-board quotelink hover preview ─────────────────────────────────────

(function () {
  var _cbCache = {};
  var _cbInFlight = {};

  function getCbPopup() { return document.getElementById('ql-popup'); }

  function fetchAndShow(link, board, pid) {
    var key = board + ':' + pid;
    var popup = getCbPopup();
    if (!popup) return;
    if (_cbCache[key]) {
      popup.innerHTML = _cbCache[key].html;
      popup.style.display = 'block';
      positionCbPopup(link, popup);
      return;
    }
    if (_cbInFlight[key]) return;
    _cbInFlight[key] = true;
    popup.innerHTML = '<div style="padding:8px;color:var(--text-dim)">loading\u2026</div>';
    popup.style.display = 'block';
    positionCbPopup(link, popup);

    fetch('/api/post/' + board + '/' + pid)
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (data) {
        _cbCache[key] = { html: data.html || '', thread_id: data.thread_id || 0 };
        delete _cbInFlight[key];
        if (_cbCache[key].thread_id) {
          var directHref = '/' + board + '/thread/' + _cbCache[key].thread_id + '#p' + pid;
          document.querySelectorAll('a.crosslink[data-crossboard="' + board + '"][data-pid="' + pid + '"]')
            .forEach(function (a) { a.href = directHref; });
        }
        if (popup.style.display !== 'none') {
          popup.innerHTML = _cbCache[key].html;
          positionCbPopup(link, popup);
        }
      })
      .catch(function () {
        delete _cbInFlight[key];
        _cbCache[key] = { html: '<div style="padding:8px;color:var(--red,#f55)">Post not found</div>', thread_id: 0 };
        if (popup.style.display !== 'none') popup.innerHTML = _cbCache[key].html;
      });
  }

  function positionCbPopup(anchor, popup) {
    var rect = anchor.getBoundingClientRect();
    var pw = popup.offsetWidth || 420;
    var ph = popup.offsetHeight || 200;
    var vw = window.innerWidth;
    var vh = window.innerHeight;
    var scrollY = window.pageYOffset;
    var left = rect.left + window.pageXOffset;
    if (left + pw > vw - 10) left = Math.max(4, vw - pw - 10);
    var top;
    if (rect.bottom + ph + 8 < vh) {
      top = rect.bottom + scrollY + 8;
    } else {
      top = rect.top + scrollY - ph - 8;
    }
    popup.style.left = left + 'px';
    popup.style.top = top + 'px';
  }

  function wireCrossLinks(root) {
    root.querySelectorAll('a.crosslink[data-crossboard][data-pid]').forEach(function (link) {
      var board = link.getAttribute('data-crossboard');
      var pid = link.getAttribute('data-pid');
      if (!board || !pid) return;
      var _hideTimer = null;
      var popup = getCbPopup();
      link.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); fetchAndShow(link, board, pid); });
      link.addEventListener('mouseleave', function () {
        _hideTimer = setTimeout(function () { if (popup) popup.style.display = 'none'; }, 120);
      });
      if (popup) {
        popup.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); });
        popup.addEventListener('mouseleave', function () {
          _hideTimer = setTimeout(function () { popup.style.display = 'none'; }, 120);
        });
      }
      link.addEventListener('click', function (e) {
        e.preventDefault();
        var key = board + ':' + pid;
        function navigate(threadId) {
          window.location.href = '/' + board + '/thread/' + threadId + '#p' + pid;
        }
        if (_cbCache[key] && _cbCache[key].thread_id) { navigate(_cbCache[key].thread_id); return; }
        fetch('/api/post/' + board + '/' + pid)
          .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
          .then(function (data) { navigate(data.thread_id || 0); })
          .catch(function () { navigate(0); });
      });
    });
  }

  wireCrossLinks(document);
  var _origInsert2 = window._onNewPostsInserted;
  window._onNewPostsInserted = function (container) {
    if (_origInsert2) _origInsert2(container);
    wireCrossLinks(container);
  };
})();

// ─── Admin ban+delete ─────────────────────────────────────────────────────────

function adminBanDelete(form, pid) {
  var reason = prompt('Ban reason (leave blank for "Rule violation"):');
  if (reason === null) return false;
  var dur = prompt('Ban duration in hours (0 = permanent):');
  if (dur === null) return false;
  var hours = parseInt(dur, 10);
  if (isNaN(hours) || hours < 0) hours = 0;
  var reasonEl = document.getElementById('ban-reason-' + pid);
  var durEl = document.getElementById('ban-dur-' + pid);
  if (reasonEl) reasonEl.value = reason.trim() || 'Rule violation';
  if (durEl) durEl.value = hours;
  return confirm('Ban IP + delete post No.' + pid + '?');
}

// ─── Poll management ──────────────────────────────────────────────────────────

function addPollOption() {
  var list = document.getElementById('poll-options-list');
  if (!list) return;
  var count = list.querySelectorAll('.poll-option-row').length + 1;
  if (count > 10) return;
  var row = document.createElement('div');
  row.className = 'poll-option-row';
  row.innerHTML = '<input type="text" name="poll_option" placeholder="Option ' + count + '" maxlength="128">'
    + '<button type="button" class="poll-remove-btn" data-action="remove-poll-option">\u2715</button>';
  list.appendChild(row);
  updateRemoveButtons();
}

function removePollOption(btn) {
  btn.closest('.poll-option-row').remove();
  updateRemoveButtons();
}

function updateRemoveButtons() {
  var rows = document.querySelectorAll('#poll-options-list .poll-option-row');
  rows.forEach(function (r) {
    var btn = r.querySelector('.poll-remove-btn');
    if (btn) btn.style.display = rows.length > 2 ? 'inline' : 'none';
  });
}

// ─── Catalog sort ─────────────────────────────────────────────────────────────

function sortCatalog(mode) {
  try { sessionStorage.setItem('catalog_sort', mode); } catch (e) {}
  var grid = document.getElementById('catalog-grid');
  if (!grid) return;
  var items = Array.from(grid.querySelectorAll('.catalog-item'));
  items.sort(function (a, b) {
    if (mode === 'bump') {
      var as_ = parseInt(a.dataset.sticky) || 0;
      var bs_ = parseInt(b.dataset.sticky) || 0;
      if (as_ !== bs_) return bs_ - as_;
      return parseInt(b.dataset.bumped) - parseInt(a.dataset.bumped);
    }
    if (mode === 'replies') return parseInt(b.dataset.replies) - parseInt(a.dataset.replies);
    if (mode === 'created') return parseInt(b.dataset.created) - parseInt(a.dataset.created);
    if (mode === 'last_reply') return parseInt(b.dataset.bumped) - parseInt(a.dataset.bumped);
    return 0;
  });
  var frag = document.createDocumentFragment();
  items.forEach(function (item) { frag.appendChild(item); });
  grid.appendChild(frag);
}

// Restore saved catalog sort on page load
(function () {
  try {
    var saved = sessionStorage.getItem('catalog_sort');
    if (saved) {
      var sel = document.getElementById('catalog-sort');
      if (sel) { sel.value = saved; sortCatalog(saved); }
    }
  } catch (e) {}
})();

// ─── PoW CAPTCHA solver ───────────────────────────────────────────────────────
// Dynamic values (board name, difficulty) are read from data-pow-board and
// data-pow-difficulty attributes on each input[name="pow_nonce"] element.

(function () {
  function sha256Fallback(str) {
    var msg = new TextEncoder().encode(str);
    var K = [
      0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
      0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
      0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
      0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
      0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
      0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
      0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
      0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
    ];
    var H = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    var len = msg.length, bitLen = len * 8;
    var padded = new Uint8Array(((len + 9 + 63) & ~63));
    padded.set(msg);
    padded[len] = 0x80;
    var dv = new DataView(padded.buffer);
    dv.setUint32(padded.length - 4, bitLen >>> 0, false);
    var r = function (n, x) { return (x >>> n) | (x << (32 - n)); };
    for (var i = 0; i < padded.length; i += 64) {
      var w = new Array(64);
      for (var j = 0; j < 16; j++) w[j] = dv.getUint32(i + j * 4, false);
      for (var j2 = 16; j2 < 64; j2++) {
        var s0 = r(7, w[j2 - 15]) ^ r(18, w[j2 - 15]) ^ (w[j2 - 15] >>> 3);
        var s1 = r(17, w[j2 - 2]) ^ r(19, w[j2 - 2]) ^ (w[j2 - 2] >>> 10);
        w[j2] = (w[j2 - 16] + s0 + w[j2 - 7] + s1) >>> 0;
      }
      var a = H[0], b = H[1], c = H[2], d = H[3], e = H[4], f = H[5], g = H[6], h = H[7];
      for (var k = 0; k < 64; k++) {
        var S1 = r(6, e) ^ r(11, e) ^ r(25, e);
        var ch = (e & f) ^ (~e & g);
        var tmp1 = (h + S1 + ch + K[k] + w[k]) >>> 0;
        var S0 = r(2, a) ^ r(13, a) ^ r(22, a);
        var maj = (a & b) ^ (a & c) ^ (b & c);
        var tmp2 = (S0 + maj) >>> 0;
        h = g; g = f; f = e; e = (d + tmp1) >>> 0; d = c; c = b; b = a; a = (tmp1 + tmp2) >>> 0;
      }
      H[0] = (H[0] + a) >>> 0; H[1] = (H[1] + b) >>> 0; H[2] = (H[2] + c) >>> 0; H[3] = (H[3] + d) >>> 0;
      H[4] = (H[4] + e) >>> 0; H[5] = (H[5] + f) >>> 0; H[6] = (H[6] + g) >>> 0; H[7] = (H[7] + h) >>> 0;
    }
    var out = new Uint8Array(32);
    var odv = new DataView(out.buffer);
    for (var ii = 0; ii < 8; ii++) odv.setUint32(ii * 4, H[ii], false);
    return Promise.resolve(out.buffer);
  }

  function sha256(str) {
    if (typeof crypto !== 'undefined' && crypto.subtle) {
      return crypto.subtle.digest('SHA-256', new TextEncoder().encode(str));
    }
    return sha256Fallback(str);
  }

  function countLeadingZeroBits(buf) {
    var bytes = new Uint8Array(buf);
    var count = 0;
    for (var i = 0; i < bytes.length; i++) {
      if (bytes[i] === 0) { count += 8; }
      else { count += Math.clz32(bytes[i]) - 24; break; }
    }
    return count;
  }

  async function startPoW(nonceEl, statusEl, board, difficulty) {
    var minute = Math.floor(Date.now() / 1000 / 60);
    var challenge = board + ':' + minute;
    var nonce = 0;
    while (true) {
      var buf = await sha256(challenge + ':' + nonce);
      if (countLeadingZeroBits(buf) >= difficulty) {
        nonceEl.value = nonce;
        if (statusEl) statusEl.textContent = '\u2713 captcha solved';
        return;
      }
      nonce++;
      if (nonce % 50000 === 0) {
        if (statusEl) statusEl.textContent = 'solving\u2026 ' + nonce.toLocaleString() + ' attempts';
        await new Promise(function (r) { setTimeout(r, 0); });
      }
    }
  }

  document.querySelectorAll('input[name="pow_nonce"]').forEach(function (nonceEl) {
    var difficulty = parseInt(nonceEl.dataset.powDifficulty, 10);
    var board = nonceEl.dataset.powBoard;
    if (!board || !difficulty) return;
    var statusId = nonceEl.id.replace('pow-nonce-', 'captcha-status-');
    var statusEl = document.getElementById(statusId);
    startPoW(nonceEl, statusEl, board, difficulty).catch(function (e) {
      if (statusEl) statusEl.textContent = 'captcha error: ' + e;
    });
  });
})();

// ─── Centralised event delegation ────────────────────────────────────────────
// Replaces all inline onclick=/onchange=/onsubmit= attribute handlers.

document.addEventListener('click', function (e) {
  // data-action handlers
  var t = e.target.closest('[data-action]');
  if (t) {
    switch (t.dataset.action) {
      case 'toggle-post-form':    togglePostForm(); break;
      case 'toggle-mobile-drawer': toggleMobileDrawer(); break;
      case 'dismiss-compress':    dismissCompressModal(); break;
      case 'start-compress':      startCompress(); break;
      case 'close-report':        closeReportModal(); break;
      case 'toggle-theme-picker': window.toggleThemePicker && window.toggleThemePicker(); break;
      case 'set-theme':           window.setTheme && window.setTheme(t.dataset.theme); break;
      case 'remove-poll-option':  removePollOption(t); break;
      case 'add-poll-option':     addPollOption(); break;
      case 'append-reply':
        e.preventDefault();
        appendReply(t.dataset.id);
        break;
      case 'expand-media':        expandMedia(t); break;
      case 'collapse-media':      collapseMedia(t); break;
      case 'fetch-updates':       window.fetchUpdates && window.fetchUpdates(); break;
      case 'open-report':
        openReportModal(t.dataset.pid, t.dataset.tid, t.dataset.board, t.dataset.csrf);
        break;
    }
  }

  // data-confirm: prompt before allowing click/submit
  var confirmEl = e.target.closest('[data-confirm]');
  if (confirmEl && !e._rcConfirmDone) {
    if (!confirm(confirmEl.dataset.confirm)) {
      e.preventDefault();
      e.stopPropagation();
    }
  }
});

document.addEventListener('change', function (e) {
  var target = e.target;
  // File inputs: check size
  if (target.name === 'file' || target.name === 'audio_file') {
    window.checkFileSize && window.checkFileSize(target);
  }
  // Autoupdate toggle
  if (target.id === 'autoupdate-toggle-cb') {
    window._toggleAutoUpdate && window._toggleAutoUpdate(target);
  }
  // Catalog sort
  if (target.id === 'catalog-sort') {
    sortCatalog(target.value);
  }
  // Allow-editing checkbox: show/hide edit-window row
  if (target.name === 'allow_editing') {
    var row = target.closest('form') && target.closest('form').querySelector('.edit-window-row');
    if (row) row.style.display = target.checked ? '' : 'none';
  }
});

document.addEventListener('submit', function (e) {
  var form = e.target;
  // data-confirm-submit: prompt before form submission
  if (form.dataset.confirmSubmit) {
    if (!confirm(form.dataset.confirmSubmit)) {
      e.preventDefault();
      return;
    }
  }
  // data-ban-delete: admin ban+delete form
  if (form.dataset.banDeletePid) {
    var pid = form.dataset.banDeletePid;
    if (!adminBanDelete(form, pid)) {
      e.preventDefault();
    }
  }
});

// ─── Report modal backdrop click ──────────────────────────────────────────────
document.addEventListener('click', function (e) {
  var modal = document.getElementById('report-modal');
  if (modal && e.target === modal) closeReportModal();
});

// ─── Appeal page: fill CSRF from cookie ──────────────────────────────────────
// Replaces the inline <script> that was previously on the ban/appeal page.
(function () {
  var field = document.getElementById('appeal-csrf-field');
  if (!field) return;
  var c = document.cookie.split('; ').find(function (r) { return r.startsWith('csrf_token='); });
  if (c) field.value = c.split('=')[1];
})();

// ─── File input size check (data-onchange-check-size) ────────────────────────
// Previously wired via onchange="checkFileSize(this)".  Now applied to all
// file inputs that carry the data-onchange-check-size attribute.
document.querySelectorAll('input[type="file"][data-onchange-check-size]').forEach(function (inp) {
  inp.addEventListener('change', function () {
    window.checkFileSize && window.checkFileSize(inp);
  });
});
