// main.js — RustChan client-side logic
// FIX[NEW-H1]: All JavaScript has been moved from inline <script> tags to this
// external file, removing the need for 'unsafe-inline' in the CSP script-src
// directive. Dynamic per-page values are passed via data-* attributes on HTML
// elements and read here at runtime.

'use strict';

var MOBILE_FORM_BREAKPOINT_PX = 700;
var POST_SUBMIT_ANCHOR_STORAGE_KEY = 'rustchanPostSubmitAnchor';

document.documentElement.classList.remove('no-js');
document.documentElement.classList.add('js');

function isMobileViewport() {
  return (
    window.matchMedia &&
    window.matchMedia('(max-width: ' + MOBILE_FORM_BREAKPOINT_PX + 'px)').matches
  );
}

function isTouchLikeDevice() {
  return (
    (window.matchMedia && window.matchMedia('(hover: none), (pointer: coarse)').matches) ||
    (navigator.maxTouchPoints || 0) > 0
  );
}

function syncMobileHeaderOffset() {
  var header = document.querySelector('.site-header');
  if (!header) return;
  var headerHeight = Math.ceil(header.getBoundingClientRect().height) + 'px';
  document.documentElement.style.setProperty(
    '--mobile-header-offset',
    headerHeight
  );
  document.documentElement.style.setProperty('--header-offset', headerHeight);
}

function syncPostFormState() {
  var wrap = document.getElementById('post-form-wrap');
  var btns = document.querySelectorAll('.post-toggle-btn[data-action="toggle-post-form"]');
  if (!wrap || !btns.length) return;
  var open = !wrap.hidden && wrap.style.display !== 'none' && !wrap.classList.contains('is-collapsed');
  wrap.classList.toggle('is-open', open);
  wrap.classList.toggle('is-collapsed', !open);
  btns.forEach(function (btn) {
    btn.classList.toggle('active', open);
    btn.setAttribute('aria-expanded', open ? 'true' : 'false');
  });
}

function setPostFormOpen(open, opts) {
  var wrap = document.getElementById('post-form-wrap');
  if (!wrap) return;
  wrap.hidden = !open;
  wrap.style.display = open ? 'block' : 'none';
  wrap.classList.toggle('is-open', open);
  wrap.classList.toggle('is-collapsed', !open);
  syncPostFormState();
  if (open) {
    var first = wrap.querySelector('input[type="text"], textarea');
    if (first && !isMobileViewport()) first.focus();
    if (isMobileViewport() || (opts && opts.scrollIntoView)) {
      setTimeout(function () {
        wrap.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }, 40);
    }
  }
}

function queuePostSubmitAnchor(target) {
  if (!target || !target.hash) return;
  try {
    window.sessionStorage.setItem(
      POST_SUBMIT_ANCHOR_STORAGE_KEY,
      JSON.stringify({
        path: target.pathname + target.search,
        hash: target.hash
      })
    );
  } catch (e) {}
}

function applyQueuedPostSubmitAnchor() {
  var raw = '';
  try {
    raw = window.sessionStorage.getItem(POST_SUBMIT_ANCHOR_STORAGE_KEY) || '';
  } catch (e) {
    return;
  }
  if (!raw) return;

  var payload = parseJsonText(raw);
  if (!payload || !payload.path || !payload.hash) return;
  if (payload.path !== window.location.pathname + window.location.search) return;

  try {
    window.sessionStorage.removeItem(POST_SUBMIT_ANCHOR_STORAGE_KEY);
  } catch (e) {}

  if (window.location.hash !== payload.hash) {
    window.location.hash = payload.hash;
  }
}

// ─── Localize post timestamps to device timezone ──────────────────────────────

function localizePostTimes(root) {
  var els = (root || document).querySelectorAll(
    'span.post-time[data-utc], span.post-edited[data-utc]'
  );
  var days = ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];
  els.forEach(function (el) {
    var ts = parseInt(el.getAttribute('data-utc'), 10);
    if (isNaN(ts)) return;
    var d = new Date(ts * 1000);
    var mm  = String(d.getMonth() + 1).padStart(2, '0');
    var dd  = String(d.getDate()).padStart(2, '0');
    var yy  = String(d.getFullYear()).slice(-2);
    var day = days[d.getDay()];
    var hh  = String(d.getHours()).padStart(2, '0');
    var min = String(d.getMinutes()).padStart(2, '0');
    var ss  = String(d.getSeconds()).padStart(2, '0');
    var local = mm + '/' + dd + '/' + yy + '(' + day + ')' + hh + ':' + min + ':' + ss;
    if (el.classList.contains('post-edited')) {
      el.title = 'last edited ' + local;
      el.textContent = '(edited ' + local + ')';
    } else {
      el.textContent = local;
    }
    el.removeAttribute('data-utc'); // prevent double-processing
  });
}

function upgradeLegacySpoilers(root) {
  (root || document).querySelectorAll('.spoiler:not([data-action])').forEach(function (el) {
    // Older posts were rendered with inline onclick handlers that are blocked by CSP.
    el.dataset.action = 'toggle-spoiler';
    el.removeAttribute('onclick');
  });
}

document.addEventListener('DOMContentLoaded', function () {
  applyQueuedPostSubmitAnchor();
  localizePostTimes(document);
  upgradeLegacySpoilers(document);
  wireAudioMiniPlayers(document);
  syncMobileHeaderOffset();

  if (window.ResizeObserver) {
    var header = document.querySelector('.site-header');
    if (header) {
      var observer = new ResizeObserver(syncMobileHeaderOffset);
      observer.observe(header);
    }
  }
});

window.addEventListener('resize', syncMobileHeaderOffset);

// Hook into new-post insertions (thread auto-update, quote popups, etc.)
(function () {
  var _origLocalize = window._onNewPostsInserted;
  window._onNewPostsInserted = function (container) {
    localizePostTimes(container);
    upgradeLegacySpoilers(container);
    wireAudioMiniPlayers(container);
    if (_origLocalize) _origLocalize(container);
  };
}());

// ─── Post form toggle & mobile drawer ────────────────────────────────────────

function togglePostForm() {
  var wrap = document.getElementById('post-form-wrap');
  if (!wrap) return;
  var opening = wrap.hidden || wrap.style.display === 'none' || wrap.classList.contains('is-collapsed');
  if (opening) {
    clearRestoredAutoQuoteOnlyDraft();
  }
  setPostFormOpen(opening);
}

function getReplyBodyField() {
  return document.getElementById('reply-body');
}

function getReplyDraftStorageKey() {
  var cfg = document.getElementById('thread-config');
  if (!cfg) return '';
  return cfg.dataset.draftKey || '';
}

function getReplyDraftMetaKey() {
  var draftKey = getReplyDraftStorageKey();
  return draftKey ? draftKey + ':mode' : '';
}

function getReplyDraftSubmitStateKey() {
  var draftKey = getReplyDraftStorageKey();
  return draftKey ? draftKey + ':submitted' : '';
}

function isQuoteOnlyReplyDraft(value) {
  if (!value) return false;
  var trimmed = value.trim();
  if (!trimmed) return false;
  return trimmed.split('\n').every(function (line) {
    var candidate = line.trim();
    return (
      !candidate ||
      /^>>\d+$/.test(candidate) ||
      /^>>>\/[a-z0-9]+\/\d+$/.test(candidate)
    );
  });
}

function getReplyDraftMode() {
  var ta = getReplyBodyField();
  if (!ta) return '';
  return ta.dataset.draftMode || '';
}

function setReplyDraftMode(mode) {
  var ta = getReplyBodyField();
  if (!ta) return;
  ta.dataset.draftMode = mode || '';
}

function isReplyDraftSubmitting() {
  var ta = getReplyBodyField();
  if (!ta) return false;
  return ta.dataset.draftSubmitting === '1';
}

function setReplyDraftSubmitting(submitting) {
  var ta = getReplyBodyField();
  if (!ta) return;
  ta.dataset.draftSubmitting = submitting ? '1' : '';
}

function markReplyDraftSubmitted() {
  var submitKey = getReplyDraftSubmitStateKey();
  if (!submitKey) return;
  try {
    sessionStorage.setItem(submitKey, '1');
  } catch (e) {}
}

function clearReplyDraftSubmitState() {
  var submitKey = getReplyDraftSubmitStateKey();
  if (!submitKey) return;
  try {
    sessionStorage.removeItem(submitKey);
  } catch (e) {}
}

function clearReplyDraftStorage() {
  var draftKey = getReplyDraftStorageKey();
  var metaKey = getReplyDraftMetaKey();
  try {
    if (draftKey) localStorage.removeItem(draftKey);
    if (metaKey) localStorage.removeItem(metaKey);
  } catch (e) {}
  var ta = getReplyBodyField();
  if (ta) {
    ta.dataset.lastPersistedDraft = '';
    ta.dataset.lastPersistedDraftMode = '';
  }
}

function persistReplyDraftStorage(force) {
  var ta = getReplyBodyField();
  var draftKey = getReplyDraftStorageKey();
  var metaKey = getReplyDraftMetaKey();
  if (!ta || !draftKey) return;
  var mode = getReplyDraftMode();
  var value = ta.value || '';
  if (document.hidden && !force) return;
  if (ta.dataset.lastPersistedDraft === value && ta.dataset.lastPersistedDraftMode === mode) {
    return;
  }
  try {
    if (value) {
      localStorage.setItem(draftKey, value);
      if (mode) {
        localStorage.setItem(metaKey, mode);
      } else {
        localStorage.removeItem(metaKey);
      }
    } else {
      clearReplyDraftStorage();
      return;
    }
    ta.dataset.lastPersistedDraft = value;
    ta.dataset.lastPersistedDraftMode = mode;
  } catch (e) {}
}

function flushReplyDraftStorage() {
  var ta = getReplyBodyField();
  if (!ta) return;
  if (ta._replyDraftSaveTimer) {
    window.clearTimeout(ta._replyDraftSaveTimer);
    ta._replyDraftSaveTimer = null;
  }
  persistReplyDraftStorage(true);
}

function queueReplyDraftSave() {
  var ta = getReplyBodyField();
  if (!ta) return;
  if (ta._replyDraftSaveTimer) {
    window.clearTimeout(ta._replyDraftSaveTimer);
  }
  ta._replyDraftSaveTimer = window.setTimeout(function () {
    ta._replyDraftSaveTimer = null;
    if (isReplyDraftSubmitting()) return;
    persistReplyDraftStorage();
  }, 500);
}

function consumeSubmittedReplyDraft() {
  var submitKey = getReplyDraftSubmitStateKey();
  var submitted = '';
  if (!submitKey) return;
  try {
    submitted = sessionStorage.getItem(submitKey) || '';
  } catch (e) {}
  if (submitted !== '1') return;
  clearReplyDraftSubmitState();
  if (/^#p\d+$/.test(window.location.hash)) {
    clearReplyDraftStorage();
  }
}

function clearRestoredAutoQuoteOnlyDraft() {
  var ta = getReplyBodyField();
  if (!ta) return;
  if (ta.dataset.draftRestored !== '1') return;
  if (getReplyDraftMode() !== 'auto-quote-only') return;
  ta.value = '';
  ta.dataset.draftRestored = '0';
  setReplyDraftMode('');
  clearReplyDraftStorage();
}

function appendReply(id) {
  var wrap = document.getElementById('post-form-wrap');
  if (wrap && (wrap.hidden || wrap.style.display === 'none' || wrap.classList.contains('is-collapsed'))) {
    setPostFormOpen(true, { scrollIntoView: true });
  }
  var ta = getReplyBodyField();
  if (ta) {
    var hadManualDraft =
      getReplyDraftMode() === 'manual' ||
      (!!ta.value && !isQuoteOnlyReplyDraft(ta.value));
    if (ta.value && !/\n$/.test(ta.value)) {
      ta.value += '\n';
    }
    ta.value += '>>' + id + '\n';
    ta.dataset.draftRestored = '0';
    setReplyDraftMode(hadManualDraft ? 'manual' : 'auto-quote-only');
    queueReplyDraftSave();
    if (!isMobileViewport()) ta.focus();
  }
  return false;
}

document.addEventListener('DOMContentLoaded', syncPostFormState);

function formatBytes(bytes) {
  if (typeof bytes !== 'number' || !isFinite(bytes) || bytes < 0) return '0 B';
  if (bytes < 1024) return bytes + ' B';
  var units = ['KiB', 'MiB', 'GiB'];
  var value = bytes / 1024;
  var unitIndex = 0;
  while (value >= 1024 && unitIndex + 1 < units.length) {
    value /= 1024;
    unitIndex += 1;
  }
  return value.toFixed(value >= 10 ? 0 : 1) + ' ' + units[unitIndex];
}

function fileInputsHaveSelection(form) {
  var fileInputs = form.querySelectorAll('input[type="file"]');
  for (var i = 0; i < fileInputs.length; i += 1) {
    if (fileInputs[i].files && fileInputs[i].files.length > 0) return true;
  }
  return false;
}

function setUploadProgress(form, percent, message) {
  var row = form.querySelector('.upload-progress-row');
  if (!row) return;
  row.hidden = false;
  var bar = row.querySelector('.upload-progress-bar');
  var text = row.querySelector('.upload-progress-text');
  if (bar && typeof percent === 'number' && isFinite(percent)) {
    var clamped = Math.max(0, Math.min(100, percent));
    bar.style.width = clamped + '%';
  }
  if (text && message) text.textContent = message;
}

function resetUploadProgress(form) {
  var row = form.querySelector('.upload-progress-row');
  if (!row) return;
  row.hidden = true;
  var bar = row.querySelector('.upload-progress-bar');
  var text = row.querySelector('.upload-progress-text');
  if (bar) bar.style.width = '0%';
  if (text) text.textContent = 'Preparing upload…';
}

function getFormSubmitButtons(form) {
  return Array.prototype.slice.call(form.querySelectorAll('button[type="submit"]'));
}

function rememberButtonLabels(buttons, labelKey) {
  buttons.forEach(function (button) {
    if (!button.dataset[labelKey]) {
      button.dataset[labelKey] = button.textContent;
    }
  });
}

function restoreButtonLabels(buttons, labelKey) {
  buttons.forEach(function (button) {
    if (button.dataset[labelKey]) {
      button.textContent = button.dataset[labelKey];
    }
  });
}

function setButtonCollectionBusy(buttons, busy, options) {
  options = options || {};
  var labelKey = options.labelKey || 'asyncOriginalLabel';
  rememberButtonLabels(buttons, labelKey);

  buttons.forEach(function (button) {
    button.disabled = !!busy;
    if (busy) {
      if (options.busyLabel) button.textContent = options.busyLabel;
    } else if (!options.preserveBusyLabel) {
      restoreButtonLabels([button], labelKey);
    }
  });
}

function setFormSubmitButtonsBusy(form, busy, options) {
  setButtonCollectionBusy(getFormSubmitButtons(form), busy, options);
}

function setSubmittingState(form, submitting) {
  form.dataset.uploadSubmitting = submitting ? '1' : '';
  setFormSubmitButtonsBusy(form, submitting, { labelKey: 'uploadOriginalLabel' });
}

function startSubmitButtonAnimation(form) {
  stopSubmitButtonAnimation(form);

  var frame = 0;
  var labels = ['Posting', 'Posting.', 'Posting..', 'Posting...'];
  var buttons = Array.prototype.slice.call(form.querySelectorAll('button[type="submit"]'));
  if (!buttons.length) return;

  function render() {
    var label = labels[frame];
    buttons.forEach(function (button) {
      button.textContent = label;
    });
    frame = (frame + 1) % labels.length;
  }

  render();
  form._submitButtonAnimationTimer = window.setInterval(render, 900);
}

function setSubmitButtonsWaitingForServer(form) {
  stopSubmitButtonAnimation(form);
  setFormSubmitButtonsBusy(form, true, {
    labelKey: 'uploadOriginalLabel',
    busyLabel: 'Upload sent, waiting for server'
  });
}

function stopSubmitButtonAnimation(form) {
  if (form._submitButtonAnimationTimer) {
    window.clearInterval(form._submitButtonAnimationTimer);
    form._submitButtonAnimationTimer = null;
  }

  restoreButtonLabels(getFormSubmitButtons(form), 'uploadOriginalLabel');
}

function dispatchPostFormEvent(form, name) {
  if (!form || !name) return;
  var ev = null;
  if (typeof window.Event === 'function') {
    ev = new Event(name, { bubbles: false, cancelable: false });
  } else if (document.createEvent) {
    ev = document.createEvent('Event');
    ev.initEvent(name, false, false);
  }
  if (!ev) return;
  form.dispatchEvent(ev);
}

function normalizeInlineMessage(message) {
  if (!message) return '';
  return String(message).replace(/\s+/g, ' ').trim();
}

function parseJsonText(text) {
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch (e) {
    return null;
  }
}

function absoluteUrl(url) {
  if (!url) return '';
  try {
    return new URL(url, window.location.href).toString();
  } catch (_err) {
    return '';
  }
}

function isSameDocumentNavigationTarget(url) {
  var target = absoluteUrl(url);
  var current = absoluteUrl(window.location.href);
  if (!target || !current) return false;

  try {
    var targetUrl = new URL(target);
    var currentUrl = new URL(current);
    return (
      targetUrl.origin === currentUrl.origin &&
      targetUrl.pathname === currentUrl.pathname &&
      targetUrl.search === currentUrl.search
    );
  } catch (_err) {
    return false;
  }
}

function clearSuccessfulPostFormState(form) {
  if (!form) return;

  clearPostFormFeedback(form);
  stopSubmitButtonAnimation(form);
  setSubmittingState(form, false);
  resetUploadProgress(form);

  if (typeof form.reset === 'function') {
    form.reset();
  }

  form.querySelectorAll('input[type="file"]').forEach(function (input) {
    try {
      input.value = '';
    } catch (e) {}
  });

  var bodyField = form.querySelector('textarea[name="body"]');
  if (bodyField) {
    bodyField.dataset.draftRestored = '0';
    bodyField.dataset.draftSubmitting = '';
  }

  setReplyDraftSubmitting(false);
  clearReplyDraftSubmitState();
  clearReplyDraftStorage();
  if (bodyField) {
    setReplyDraftMode('');
  }
}

function navigatePostSubmitTarget(form, url) {
  if (!url) return false;

  // Upload-backed replies redirect back to the same thread with a fresh #p123
  // anchor. A plain hash navigation does not fetch the newly-created post, so
  // force a full navigation and restore the anchor after load.
  // Reset the live form first so browsers do not carry the just-submitted text
  // or file input selection across that navigation.
  var sameDocument = isSameDocumentNavigationTarget(url);
  if (sameDocument) {
    clearSuccessfulPostFormState(form);
    var target = new URL(url, window.location.href);
    queuePostSubmitAnchor(target);
    window.location.assign(target.pathname + target.search);
    return true;
  }
  window.location.assign(url);
  return true;
}

function parseXhrJsonPayload(xhr) {
  if (!xhr || !xhr.responseText) return null;
  var contentType = xhr.getResponseHeader('Content-Type') || '';
  if (contentType.indexOf('application/json') === -1) return null;
  return parseJsonText(xhr.responseText);
}

function extractMessageFromHtmlDocument(html) {
  if (!html || typeof DOMParser !== 'function') return '';
  try {
    var doc = new DOMParser().parseFromString(html, 'text/html');
    if (!doc) return '';

    var banner = doc.querySelector('.post-error-banner');
    if (banner) return normalizeInlineMessage(banner.textContent);

    var bannedHeading = doc.querySelector('.error-page h1');
    if (bannedHeading && /you are banned/i.test(bannedHeading.textContent || '')) {
      var reason = doc.querySelector('.error-page strong');
      if (reason) {
        return normalizeInlineMessage('You are banned. Reason: ' + reason.textContent);
      }
      return normalizeInlineMessage(bannedHeading.textContent);
    }

    var errorPage = doc.querySelector('.page-box.error-page p');
    if (errorPage) return normalizeInlineMessage(errorPage.textContent);
  } catch (e) {}
  return '';
}

function clearPostFormFeedback(form) {
  var container = form && (form.closest('.post-form-container') || form);
  if (!container) return;
  container
    .querySelectorAll('.post-error-banner[data-post-form-feedback="1"]')
    .forEach(function (banner) {
      if (banner.parentNode) banner.parentNode.removeChild(banner);
    });
}

function showPostFormFeedback(form, message) {
  var normalized = normalizeInlineMessage(message);
  var container = form && (form.closest('.post-form-container') || form);
  if (!normalized || !container) return;

  clearPostFormFeedback(form);

  var banner = document.createElement('div');
  banner.className = 'post-error-banner';
  banner.dataset.postFormFeedback = '1';
  banner.setAttribute('role', 'alert');
  banner.setAttribute('tabindex', '-1');
  banner.textContent = '\u26A0 ' + normalized;

  var title = container.querySelector('.post-form-title');
  if (title && title.parentNode === container) {
    title.insertAdjacentElement('afterend', banner);
  } else if (form && form.parentNode === container) {
    container.insertBefore(banner, form);
  } else {
    container.insertBefore(banner, container.firstChild);
  }

  try {
    banner.focus({ preventScroll: true });
  } catch (e) {
    banner.focus();
  }
  if (typeof banner.scrollIntoView === 'function') {
    banner.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  }
}

function resetPostSubmitFailureState(form, message) {
  stopSubmitButtonAnimation(form);
  setSubmittingState(form, false);
  resetUploadProgress(form);
  dispatchPostFormEvent(form, 'rustchan:post-submit-reset');
  showPostFormFeedback(form, message);
}

function submitPostFormWithProgress(form) {
  if (!window.XMLHttpRequest || form.dataset.uploadSubmitting === '1') return false;
  if (!fileInputsHaveSelection(form)) return false;

  var xhr = new XMLHttpRequest();
  var submitHelper = createAsyncSubmitHelper({
    form: form,
    labelKey: 'uploadOriginalLabel',
    onBusyStart: function () {
      setSubmittingState(form, true);
      startSubmitButtonAnimation(form);
    },
    onBusyEnd: function () {
      stopSubmitButtonAnimation(form);
      setSubmittingState(form, false);
    },
    setProgress: function (percent, message) {
      setUploadProgress(form, percent, message);
    }
  });
  xhr.open((form.method || 'POST').toUpperCase(), form.action, true);
  xhr.setRequestHeader('X-Requested-With', 'XMLHttpRequest');

  clearPostFormFeedback(form);
  submitHelper.setBusy(true);
  submitHelper.setProgress(0, 'Starting upload…');

  xhr.upload.addEventListener('progress', function (event) {
    if (event.lengthComputable && event.total > 0) {
      var percent = (event.loaded / event.total) * 100;
      if (event.loaded >= event.total) {
        setSubmitButtonsWaitingForServer(form);
      }
      submitHelper.setProgress(
        percent,
        'Uploading ' + formatBytes(event.loaded) + ' / ' + formatBytes(event.total) + ' (' + Math.round(percent) + '%)'
      );
    } else {
      submitHelper.setProgress(100, 'Uploading…');
    }
  });

  xhr.addEventListener('load', function () {
    var payload = submitHelper.parsePayload(xhr);
    var explicitRedirect = submitHelper.extractRedirect(xhr, payload);

    submitHelper.setBusy(false);
    submitHelper.setProgress(100, 'Finishing…');

    // XHR follows redirects internally, and some browsers expose the final
    // response URL without the original #p123 fragment. The explicit redirect
    // header keeps reply-draft clearing and "(You)" tracking anchored to the
    // exact new post after upload-backed replies succeed.
    if (explicitRedirect) {
      navigatePostSubmitTarget(form, explicitRedirect);
      return;
    }

    var finalUrl = absoluteUrl(xhr.responseURL || form.action);
    var currentUrl = absoluteUrl(window.location.href);

    if (xhr.status >= 200 && xhr.status < 400 && finalUrl && finalUrl !== currentUrl) {
      navigatePostSubmitTarget(form, finalUrl);
      return;
    }

    if (payload && payload.error) {
      resetPostSubmitFailureState(form, payload.error);
      return;
    }

    if (xhr.status >= 200 && xhr.status < 400) {
      window.location.reload();
      return;
    }

    resetPostSubmitFailureState(
      form,
      submitHelper.extractError(xhr, payload, 'Upload failed. Please try again.')
    );
  });

  xhr.addEventListener('error', function () {
    resetPostSubmitFailureState(
      form,
      'Connection dropped before the server response arrived. Your post may still have succeeded. Refresh the thread or board before trying again.'
    );
  });

  xhr.addEventListener('abort', function () {
    stopSubmitButtonAnimation(form);
    setSubmittingState(form, false);
    resetUploadProgress(form);
    dispatchPostFormEvent(form, 'rustchan:post-submit-reset');
  });

  xhr.send(new FormData(form));
  return true;
}

function captchaNonceMissing(form) {
  var nonceField = form && form.querySelector('input[name="pow_nonce"]');
  return !!(nonceField && !nonceField.value);
}

// ─── NSFW disclaimer overlay ────────────────────────────────────────────────

function openNsfwDisclaimer(returnTo, boardLabel) {
  var overlay = document.getElementById('nsfw-disclaimer-overlay');
  if (!overlay) return;
  var returnField = document.getElementById('nsfw-return-to');
  var boardEl = document.getElementById('nsfw-board-label');
  if (returnField && returnTo) returnField.value = returnTo;
  if (boardEl) boardEl.textContent = boardLabel || '';
  overlay.hidden = false;
  overlay.classList.add('is-open');
  document.body.classList.add('mobile-overlay-open');
}

function closeNsfwDisclaimer() {
  var overlay = document.getElementById('nsfw-disclaimer-overlay');
  if (!overlay) return;
  overlay.hidden = true;
  overlay.classList.remove('is-open');
  document.body.classList.remove('mobile-overlay-open');
  if (window.location.pathname === '/' && window.location.search.indexOf('nsfw=') !== -1 && window.history && window.history.replaceState) {
    window.history.replaceState({}, document.title, '/');
  }
}

document.addEventListener('DOMContentLoaded', function () {
  var overlay = document.getElementById('nsfw-disclaimer-overlay');
  if (overlay && !overlay.hidden) {
    document.body.classList.add('mobile-overlay-open');
  }
});

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
    expanded.setAttribute('playsinline', '');
    expanded.setAttribute('webkit-playsinline', '');
    expanded.play().catch(function () {});
  }
  syncComboAudio(container, true);
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
  // Clear the inline display override so CSS can restore the thumbnail
  // preview to its natural inline-block hit area.
  preview.style.display = '';
  btn.style.display = 'none';
}

function syncComboAudio(container, shouldPlay) {
  if (!container || !container.classList.contains('image-audio-combo')) return;
  var audio = container.querySelector('.audio-player-combo');
  if (!audio) return;
  if (shouldPlay) {
    audio.play().catch(function () {});
  }
}

function preferredMiniPlayerArtwork() {
  var audioArtworkLink = document.querySelector(
    'link[rel="apple-touch-icon"], link[rel="icon"][sizes="192x192"], link[rel="icon"][sizes="512x512"], link[rel="icon"][sizes="32x32"], link[rel="icon"]'
  );
  if (!audioArtworkLink || !audioArtworkLink.href) return [];
  var artwork = { src: audioArtworkLink.href };
  if (audioArtworkLink.sizes && audioArtworkLink.sizes.value) {
    artwork.sizes = audioArtworkLink.sizes.value;
  }
  if (audioArtworkLink.type) {
    artwork.type = audioArtworkLink.type;
  }
  return [artwork];
}

function audioMiniPlayerArtwork(audio) {
  var artworkSrc = audio.dataset.artworkSrc;
  if (!artworkSrc) return preferredMiniPlayerArtwork();
  return [{ src: new URL(artworkSrc, window.location.href).href }];
}

function updateAudioMiniPlayer(audio) {
  if (!audio || !('mediaSession' in navigator) || typeof window.MediaMetadata !== 'function') {
    return;
  }
  var source = audio.querySelector('source');
  var sourcePath = source && source.getAttribute('src');
  var title = audio.dataset.audioTitle || (sourcePath ? sourcePath.split('/').pop() : document.title);
  var metadata = {
    title: title,
    album: document.title
  };
  var artwork = audioMiniPlayerArtwork(audio);
  if (artwork.length) metadata.artwork = artwork;
  navigator.mediaSession.metadata = new MediaMetadata(metadata);
}

function wireAudioMiniPlayers(root) {
  (root || document).querySelectorAll('audio.audio-player').forEach(function (audio) {
    if (audio.dataset.miniplayerWired === '1') return;
    audio.dataset.miniplayerWired = '1';
    audio.addEventListener('play', function () {
      updateAudioMiniPlayer(audio);
    });
  });
}

function expandVideoEmbed(preview, type, id, container) {
  var src = '';
  var title = '';
  if (type === 'youtube') {
    src = 'https://www.youtube-nocookie.com/embed/' + id + '?autoplay=1&rel=0&playsinline=1';
    title = 'YouTube video player';
  } else if (type === 'streamable') {
    src = 'https://streamable.com/e/' + id + '?autoplay=1';
    title = 'Streamable player';
  }

  var iframe = document.createElement('iframe');
  if (type === 'youtube') {
    iframe.src = src;
    iframe.setAttribute('title', title);
  } else if (type === 'streamable') {
    iframe.src = src;
    iframe.setAttribute('title', title);
  }
  iframe.className = 'embed-iframe';
  iframe.setAttribute('frameborder', '0');
  iframe.setAttribute('allowfullscreen', '');
  iframe.setAttribute('allow', 'accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share; fullscreen');
  iframe.setAttribute('referrerpolicy', 'strict-origin-when-cross-origin');
  preview.style.display = 'none';
  var closeBtn = container.querySelector('.media-close-btn');
  if (closeBtn) closeBtn.style.display = 'inline-flex';
  container.classList.add('media-is-expanded');
  container.appendChild(iframe);
}

function collapseVideoEmbed(btn) {
  var container = btn.closest('.video-embed-container');
  if (!container) return;
  var iframe = container.querySelector('.embed-iframe');
  var preview = container.querySelector('.media-preview');
  if (iframe) { iframe.src = ''; iframe.remove(); }
  if (preview) preview.style.display = '';
  container.classList.remove('media-is-expanded');
  btn.style.display = 'none';
}

// ─── Auto-compress modal ─────────────────────────────────────────────────────
// Dynamic limits (MAX_IMAGE / MAX_VIDEO) are read from data-max-image /
// data-max-video attributes on the #compress-modal element, injected by the
// Rust template at render time.

function createAsyncSubmitHelper(options) {
  options = options || {};
  var form = options.form;
  var labelKey = options.labelKey || 'asyncOriginalLabel';

  function setBusy(busy, busyLabel) {
    setFormSubmitButtonsBusy(form, busy, {
      busyLabel: busyLabel || options.busyLabel || '',
      labelKey: labelKey
    });
    if (busy) {
      if (options.onBusyStart) options.onBusyStart();
    } else if (options.onBusyEnd) {
      options.onBusyEnd();
    }
  }

  return {
    setBusy: setBusy,
    setProgress: function (percent, message) {
      if (options.setProgress) options.setProgress(percent, message);
    },
    parsePayload: function (xhr) {
      return parseXhrJsonPayload(xhr);
    },
    extractRedirect: function (xhr, payload) {
      if (options.extractRedirect) return options.extractRedirect(xhr, payload);
      return (
        (xhr && xhr.getResponseHeader && xhr.getResponseHeader('X-Rustchan-Redirect')) ||
        (payload && payload.redirect_url) ||
        ''
      );
    },
    extractError: function (xhr, payload, fallback) {
      if (options.extractError) return options.extractError(xhr, payload, fallback);
      var contentType = (xhr && xhr.getResponseHeader && xhr.getResponseHeader('Content-Type')) || '';
      var isHtml = contentType.indexOf('text/html') !== -1;
      return (
        (payload && payload.error) ||
        extractMessageFromHtmlDocument(isHtml ? xhr.responseText : '') ||
        fallback
      );
    }
  };
}

function requestFormSubmit(form, submitter) {
  if (typeof form.requestSubmit === 'function') {
    if (submitter) {
      form.requestSubmit(submitter);
    } else {
      form.requestSubmit();
    }
    return;
  }
  form.submit();
}

function isDangerousConfirmationTrigger(trigger, message) {
  if (trigger && trigger.classList && trigger.classList.contains('btn-danger')) return true;
  return /warning|delete|restore|vacuum|repair/i.test(message || '');
}

var _confirmModal = null;
var _confirmCancelButton = null;
var _confirmContinueButton = null;
var _confirmMessageEl = null;
var _confirmResolve = null;
var _confirmActiveTrigger = null;

function ensureConfirmModal() {
  if (_confirmModal) return true;
  _confirmModal = document.getElementById('confirm-modal');
  if (!_confirmModal) return false;
  _confirmCancelButton = document.getElementById('confirm-modal-cancel');
  _confirmContinueButton = document.getElementById('confirm-modal-continue');
  _confirmMessageEl = document.getElementById('confirm-modal-message');
  return !!(_confirmModal && _confirmCancelButton && _confirmContinueButton && _confirmMessageEl);
}

function closeConfirmModal(confirmed) {
  if (!ensureConfirmModal() || _confirmModal.style.display === 'none') return;
  _confirmModal.style.display = 'none';
  _confirmContinueButton.classList.remove('btn-danger');
  if (!confirmed && _confirmActiveTrigger && typeof _confirmActiveTrigger.focus === 'function') {
    _confirmActiveTrigger.focus();
  }
  var resolve = _confirmResolve;
  _confirmResolve = null;
  _confirmActiveTrigger = null;
  if (resolve) resolve(!!confirmed);
}

function requestConfirmation(message, trigger, options) {
  options = options || {};
  if (!ensureConfirmModal()) return Promise.resolve(window.confirm(message));

  _confirmActiveTrigger = trigger || document.activeElement;
  _confirmMessageEl.textContent = message;
  _confirmContinueButton.classList.toggle(
    'btn-danger',
    !!options.dangerous
  );
  _confirmModal.style.display = 'flex';

  return new Promise(function (resolve) {
    _confirmResolve = resolve;
    window.setTimeout(function () {
      if (_confirmCancelButton) _confirmCancelButton.focus();
    }, 0);
  });
}

window.createAsyncSubmitHelper = createAsyncSubmitHelper;
window.requestConfirmation = requestConfirmation;

(function () {
  var _input = null, _file = null, _max = 0, _compressing = false;
  var VIDEO_COMPRESS_TIMEOUT_MS = 120000;

  function getMax(type) {
    var modal = document.getElementById('compress-modal');
    if (!modal) return 0;
    if (type === 'image') return parseInt(modal.dataset.maxImage, 10) || 0;
    if (type === 'video') return parseInt(modal.dataset.maxVideo, 10) || 0;
    return 0;
  }

  function imageOutputType(file) {
    if (file.type === 'image/jpeg' || file.type === 'image/jpg') return 'image/jpeg';
    if (file.type === 'image/png' || file.type === 'image/webp') {
      return canvasSupportsType('image/webp') ? 'image/webp' : 'image/jpeg';
    }
    return 'image/jpeg';
  }

  function imageOutputExt(type) {
    return type === 'image/webp' ? 'webp' : 'jpg';
  }

  function canvasSupportsType(type) {
    var canvas = document.createElement('canvas');
    return canvas.toDataURL(type).indexOf('data:' + type) === 0;
  }

  function hasAnimatedWebP(bytes) {
    var marker = [0x41, 0x4e, 0x49, 0x4d]; // ANIM
    for (var i = 0; i <= bytes.length - marker.length; i += 1) {
      var matched = true;
      for (var j = 0; j < marker.length; j += 1) {
        if (bytes[i + j] !== marker[j]) {
          matched = false;
          break;
        }
      }
      if (matched) return true;
    }
    return false;
  }

  function hasAnimatedGif(bytes) {
    var frameMarkers = 0;
    for (var i = 0; i <= bytes.length - 2; i += 1) {
      if (bytes[i] === 0x21 && bytes[i + 1] === 0xf9) {
        frameMarkers += 1;
        if (frameMarkers > 1) return true;
      }
    }
    return false;
  }

  function isAnimatedImage(file) {
    if (!file || !file.arrayBuffer) return Promise.resolve(false);
    if (file.type !== 'image/gif' && file.type !== 'image/webp') return Promise.resolve(false);
    return file.arrayBuffer().then(function (buffer) {
      var bytes = new Uint8Array(buffer);
      if (file.type === 'image/gif') return hasAnimatedGif(bytes);
      if (file.type === 'image/webp') return hasAnimatedWebP(bytes);
      return false;
    }).catch(function () {
      return false;
    });
  }

  function imageHasTransparency(img) {
    var sample = document.createElement('canvas');
    var sampleCtx = sample.getContext('2d');
    if (!sampleCtx) return false;
    sample.width = Math.min(img.naturalWidth || img.width || 1, 64);
    sample.height = Math.min(img.naturalHeight || img.height || 1, 64);
    sampleCtx.drawImage(img, 0, 0, sample.width, sample.height);
    var data = sampleCtx.getImageData(0, 0, sample.width, sample.height).data;
    for (var i = 3; i < data.length; i += 4) {
      if (data[i] !== 255) return true;
    }
    return false;
  }

  function stripFileExtension(name) {
    return /\.[^.]+$/.test(name) ? name.replace(/\.[^.]+$/, '') : name;
  }

  function stopMediaStream(stream) {
    if (!stream || !stream.getTracks) return;
    stream.getTracks().forEach(function (track) {
      try { track.stop(); } catch (e) {}
    });
  }

  function cleanupVideoElement(videoEl) {
    if (!videoEl) return;
    try { videoEl.pause(); } catch (e) {}
    try {
      videoEl.removeAttribute('src');
      videoEl.load();
    } catch (e) {}
  }

  function videoRecorderMimeType() {
    if (!window.MediaRecorder) return '';
    var types = [
      'video/webm;codecs=vp9,opus',
      'video/webm;codecs=vp9',
      'video/webm;codecs=vp8,opus',
      'video/webm;codecs=vp8',
      'video/webm'
    ];
    for (var i = 0; i < types.length; i += 1) {
      if (MediaRecorder.isTypeSupported(types[i])) return types[i];
    }
    return '';
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
      var ext = isImg ? imageOutputExt(blob.type) : 'webm';
      var newName = stripFileExtension(_file.name) + '_compressed.' + ext;
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
      isAnimatedImage(file).then(function (animated) {
        if (animated) {
          reject(new Error('Animated images are not auto-compressed to avoid losing animation'));
          return;
        }
        var img = new Image();
        var url = URL.createObjectURL(file);
        img.onload = function () {
          URL.revokeObjectURL(url);
          var w = img.naturalWidth, h = img.naturalHeight;
          var scale = 1.0, quality = 0.85;
          var outputType = imageOutputType(file);
          if (outputType === 'image/jpeg' && imageHasTransparency(img)) {
            reject(new Error('This image uses transparency and this browser cannot safely auto-compress it'));
            return;
          }
          var canvas = document.createElement('canvas');
          var ctx = canvas.getContext('2d');
          if (!ctx) {
            reject(new Error('Canvas 2D context unavailable'));
            return;
          }
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
            }, outputType, quality);
          }
          tryEncode();
        };
        img.onerror = function () { URL.revokeObjectURL(url); reject(new Error('Image load failed')); };
        img.src = url;
      });
    });
  }

  function _compressVideo(file, maxBytes) {
    return new Promise(function (resolve, reject) {
      if (!window.MediaRecorder) { reject(new Error('MediaRecorder not supported')); return; }
      var mimeType = videoRecorderMimeType();
      if (!mimeType) { reject(new Error('No supported WebM encoder available in this browser')); return; }
      var url = URL.createObjectURL(file);
      var videoEl = document.createElement('video');
      videoEl.preload = 'auto';
      videoEl.muted = true;
      videoEl.playsInline = true;
      videoEl.src = url;
      var duration = 0;
      var stream = null;
      var recorder = null;
      var progressTimer = null;
      var safetyTimer = null;
      var settled = false;

      function finish(err, blob) {
        if (settled) return;
        settled = true;
        if (progressTimer) clearInterval(progressTimer);
        if (safetyTimer) clearTimeout(safetyTimer);
        stopMediaStream(stream);
        cleanupVideoElement(videoEl);
        URL.revokeObjectURL(url);
        if (err) {
          reject(err);
        } else {
          resolve(blob);
        }
      }

      videoEl.onloadedmetadata = function () {
        duration = videoEl.duration;
        if (!duration || !isFinite(duration)) { finish(new Error('Cannot determine video duration')); return; }
        _setProgress(10, 'Analysing video\u2026');
        var targetBitsPerSec = Math.floor((maxBytes * 8) / duration * 0.9);
        try {
          stream = videoEl.captureStream ? videoEl.captureStream() : videoEl.mozCaptureStream();
        } catch (e) {
          finish(e);
          return;
        }
        if (!stream) {
          finish(new Error('Video capture stream is not available'));
          return;
        }
        try {
          recorder = new MediaRecorder(stream, {
            mimeType: mimeType,
            videoBitsPerSecond: Math.max(targetBitsPerSec, 120000)
          });
        } catch (e) {
          finish(e);
          return;
        }
        var chunks = [];
        recorder.ondataavailable = function (e) { if (e.data && e.data.size > 0) chunks.push(e.data); };
        recorder.onstop = function () {
          finish(null, new Blob(chunks, { type: 'video/webm' }));
        };
        recorder.onerror = function (e) { finish(e.error || new Error('MediaRecorder error')); };
        videoEl.currentTime = 0;
        recorder.start(1000);
        progressTimer = setInterval(function () {
          _setProgress(Math.min(10 + Math.round((videoEl.currentTime / duration) * 80), 90), 'Re-encoding\u2026 ' + Math.round((videoEl.currentTime / duration) * 100) + '%');
        }, 500);
        safetyTimer = setTimeout(function () {
          if (recorder && recorder.state !== 'inactive') {
            try { recorder.stop(); } catch (e) {}
          }
          finish(new Error('Video compression timed out'));
        }, VIDEO_COMPRESS_TIMEOUT_MS);
        videoEl.addEventListener('ended', function handleEnded() {
          videoEl.removeEventListener('ended', handleEnded);
          if (recorder && recorder.state !== 'inactive') {
            try { recorder.stop(); } catch (e) { finish(e); }
          }
        });
        videoEl.play().catch(function (err) {
          finish(err || new Error('Video playback failed during compression'));
        });
      };
      videoEl.onerror = function () { finish(new Error('Video load error')); };
      videoEl.load();
    });
  }
})();

(function () {
  var ADMIN_DROPDOWN_STORAGE_PREFIX = 'rustchan_admin_dropdown:';

  function readAdminDropdownState(key) {
    try {
      return localStorage.getItem(ADMIN_DROPDOWN_STORAGE_PREFIX + key);
    } catch (e) {
      return null;
    }
  }

  function writeAdminDropdownState(key, isOpen) {
    try {
      localStorage.setItem(ADMIN_DROPDOWN_STORAGE_PREFIX + key, isOpen ? '1' : '0');
    } catch (e) {}
  }

  function initPersistentAdminDropdowns() {
    document.querySelectorAll('details.admin-dropdown[data-admin-dropdown-key]').forEach(function (details) {
      var key = details.dataset.adminDropdownKey;
      if (!key) return;

      var stored = readAdminDropdownState(key);
      if (stored === '1') {
        details.open = true;
      } else if (stored === '0') {
        details.open = false;
      }

      details.addEventListener('toggle', function () {
        writeAdminDropdownState(key, details.open);
      });
    });
  }

  document.addEventListener('DOMContentLoaded', initPersistentAdminDropdowns);
})();

(function () {
  function syncBannerTargetPicker(picker) {
    if (!picker) return;
    var select = picker.querySelector('[data-banner-target-select]');
    if (!select) return;
    var selected = select.value || 'none';
    picker.querySelectorAll('[data-banner-target-field]').forEach(function (field) {
      var matches = field.dataset.bannerTargetField === selected;
      field.hidden = !matches;
      field.querySelectorAll('input, select, textarea').forEach(function (input) {
        input.disabled = !matches;
      });
    });
  }

  function bannerWarningNode(form) {
    if (!form) return null;
    var warning = form.querySelector('[data-banner-warning]');
    if (warning) return warning;
    warning = document.createElement('div');
    warning.className = 'admin-flash flash-error admin-banner-inline-warning';
    warning.dataset.bannerWarning = '1';
    warning.hidden = true;
    form.appendChild(warning);
    return warning;
  }

  function clearBannerWarning(form) {
    var warning = bannerWarningNode(form);
    if (!warning) return;
    warning.hidden = true;
    warning.textContent = '';
  }

  function showBannerWarning(form, message) {
    var warning = bannerWarningNode(form);
    if (!warning) return;
    warning.hidden = false;
    warning.textContent = message;
  }

  function externalBannerLinksEnabled() {
    var toggle = document.querySelector('[data-banner-external-toggle]');
    return !!(toggle && toggle.checked);
  }

  function initBannerEditors(root) {
    (root || document).querySelectorAll('[data-banner-target-picker]').forEach(function (picker) {
      syncBannerTargetPicker(picker);
    });

    (root || document).querySelectorAll('form[data-banner-editor="1"]').forEach(function (form) {
      if (form.dataset.bannerEditorWired === '1') return;
      form.dataset.bannerEditorWired = '1';

      form.addEventListener('submit', function (event) {
        var select = form.querySelector('[data-banner-target-select]');
        if (!select) return;
        if (select.value !== 'external_url' || externalBannerLinksEnabled()) {
          clearBannerWarning(form);
          return;
        }
        event.preventDefault();
        showBannerWarning(
          form,
          'Enable external banner links in Global board banner settings before saving a banner that opens another website.'
        );
        var warning = bannerWarningNode(form);
        if (warning && typeof warning.scrollIntoView === 'function') {
          warning.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
        }
      });
    });
  }

  document.addEventListener('DOMContentLoaded', function () {
    initBannerEditors(document);
  });

  document.addEventListener('change', function (event) {
    var target = event.target;
    if (target.matches && target.matches('[data-banner-target-select]')) {
      var picker = target.closest('[data-banner-target-picker]');
      syncBannerTargetPicker(picker);
      clearBannerWarning(target.closest('form'));
      return;
    }
    if (target.matches && target.matches('[data-banner-external-toggle]')) {
      document.querySelectorAll('form[data-banner-editor="1"]').forEach(function (form) {
        clearBannerWarning(form);
      });
    }
  });
})();

(function () {
  function initAdminLiveLog() {
    var output = document.getElementById('admin-live-log-output');
    var fileLabel = document.getElementById('admin-live-log-file');
    var refreshBtn = document.getElementById('admin-live-log-refresh');
    var clearBtn = document.getElementById('admin-live-log-clear');
    var autoscroll = document.getElementById('admin-live-log-autoscroll');
    if (!output) return;

    var timer = null;
    var lastText = '';
    var clearedBaseline = '';
    var clearedFile = '';

    function visibleText(fullText, fileName) {
      if (!clearedBaseline || clearedFile !== fileName) {
        return fullText;
      }
      if (fullText === clearedBaseline) {
        return '';
      }
      if (fullText.indexOf(clearedBaseline) === 0) {
        return fullText.slice(clearedBaseline.length).replace(/^\n+/, '');
      }
      return fullText;
    }

    function fetchLog() {
      fetch('/admin/log/live?bytes=65536', { credentials: 'same-origin' })
        .then(function (resp) { return resp.json(); })
        .then(function (data) {
          var fileName = data.filename || 'current log';
          var fullText = data.content || '';
          if (data.truncated) {
            fullText = '[showing latest log tail]\n' + fullText;
          }
          if (fileLabel) fileLabel.textContent = fileName;
          if (fileName !== clearedFile) {
            clearedBaseline = '';
            clearedFile = fileName;
          }
          if (fullText === lastText) return;
          lastText = fullText;
          var text = visibleText(fullText, fileName);
          output.textContent = text || 'Waiting for new log lines…';
          if (!autoscroll || autoscroll.checked) {
            output.scrollTop = output.scrollHeight;
          }
        })
        .catch(function () {
          output.textContent = 'Failed to load live log.';
        });
    }

    if (refreshBtn) {
      refreshBtn.addEventListener('click', function () {
        fetchLog();
      });
    }

    if (clearBtn) {
      clearBtn.addEventListener('click', function () {
        clearedBaseline = lastText;
        clearedFile = fileLabel ? fileLabel.textContent : '';
        output.textContent = 'Waiting for new log lines…';
      });
    }

    fetchLog();
    timer = setInterval(fetchLog, 2000);

    window.addEventListener('beforeunload', function () {
      if (timer) clearInterval(timer);
    });
  }

  document.addEventListener('DOMContentLoaded', initAdminLiveLog);
})();

// ─── Report modal ─────────────────────────────────────────────────────────────

function openReportModal(postId, threadId, board, csrf, label) {
  document.getElementById('report-post-id').value = postId;
  document.getElementById('report-thread-id').value = threadId;
  document.getElementById('report-board').value = board;
  document.getElementById('report-csrf').value = csrf;
  var info = document.getElementById('report-info');
  if (info) info.textContent = label || ('Reporting post No.' + postId);
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

function closeThreadMenus() {
  document.querySelectorAll('.catalog-thread-menu-toggle[aria-expanded="true"]').forEach(function (btn) {
    btn.setAttribute('aria-expanded', 'false');
  });
  document.querySelectorAll('.catalog-thread-menu').forEach(function (menu) {
    menu.hidden = true;
  });
}

function toggleThreadMenu(toggle) {
  if (!toggle) return;
  var actions = toggle.closest('.catalog-card-actions');
  var menu = actions && actions.querySelector('.catalog-thread-menu');
  if (!menu) return;
  var opening = menu.hidden;
  closeThreadMenus();
  menu.hidden = !opening;
  toggle.setAttribute('aria-expanded', opening ? 'true' : 'false');
}

// ─── Theme picker ─────────────────────────────────────────────────────────────

(function () {
  var THEMES = (document.documentElement.getAttribute('data-theme-slugs') || '')
    .split(',')
    .filter(function (value) { return value; });

  function persistTheme(t, href) {
    var url = href || ('/theme/' + encodeURIComponent(t));
    try {
      fetch(url, {
        credentials: 'same-origin',
        headers: { 'x-rustchan-background': '1' }
      }).catch(function () {});
    } catch (e) {}
  }

  function applyThemeStylesheet(t) {
    var el = document.getElementById('active-theme-stylesheet');
    if (!t || t === 'terminal') {
      if (el) el.remove();
      return;
    }
    if (!el) {
      el = document.createElement('link');
      el.id = 'active-theme-stylesheet';
      el.rel = 'stylesheet';
      document.head.appendChild(el);
    }
    el.href = '/theme-css/' + encodeURIComponent(t);
  }

  function applyTheme(t) {
    if (t === 'terminal') {
      document.documentElement.removeAttribute('data-theme');
    } else {
      document.documentElement.setAttribute('data-theme', t);
    }
    applyThemeStylesheet(t);
    // Match by data-theme attribute so order in DOM doesn't matter.
    document.querySelectorAll('.tp-option').forEach(function (el) {
      el.classList.toggle('active', el.dataset.theme === t);
    });
  }

  window.setTheme = function (t, href) {
    try { localStorage.setItem('rustchan_theme', t); } catch (e) {}
    applyTheme(t);
    persistTheme(t, href);
    closeThemePicker();
  };

  window.toggleThemePicker = function () {
    var p = document.getElementById('theme-picker-panel');
    if (!p) return;
    var open = !p.classList.contains('open');
    p.classList.toggle('open', open);
    document.body.classList.toggle('theme-picker-open', open);
  };

  function closeThemePicker() {
    var p = document.getElementById('theme-picker-panel');
    if (p) p.classList.remove('open');
    document.body.classList.remove('theme-picker-open');
  }

  document.addEventListener('click', function (e) {
    var btn = document.getElementById('theme-picker-btn');
    var panel = document.getElementById('theme-picker-panel');
    if (btn && panel && !btn.contains(e.target) && !panel.contains(e.target)) {
      closeThemePicker();
    }
  });

  // Priority: personal localStorage preference > server-configured default.
  // The server injects data-default-theme on <html> when the admin picks a
  // non-default theme. New visitors (no localStorage) should see that theme
  // instead of falling back to terminal.
  (function () {
    var active = null;
    try { active = localStorage.getItem('rustchan_theme'); } catch (e) {}
    if (!active || THEMES.indexOf(active) === -1) {
      active = document.documentElement.getAttribute('data-default-theme') || 'fluorogrid';
    }
    if (active && THEMES.indexOf(active) !== -1) { applyTheme(active); }
  }());
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
  var statusEls = Array.prototype.slice.call(
    document.querySelectorAll('[data-role="autoupdate-status"]')
  );
  var toggleEls = Array.prototype.slice.call(
    document.querySelectorAll('[data-role="autoupdate-toggle"]')
  );
  var timer = null;
  var updating = false;
  var autoOn = false;

  if (!container) return;

  var board = container.dataset.board;
  var threadId = container.dataset.threadId;
  var lastId = parseInt(container.dataset.lastId, 10) || 0;
  // Track the board-list version last seen so we only touch the DOM when it
  // actually changes (avoids unnecessary reflow on every poll tick).
  var lastBoardsVersion = -1;

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

  var updateButtons = Array.prototype.slice.call(
    document.querySelectorAll('[data-action="fetch-updates"]')
  );
  var statusTimer = null;

  function setStatus(msg, options) {
    options = options || {};
    if (statusTimer) {
      window.clearTimeout(statusTimer);
      statusTimer = null;
    }
    statusEls.forEach(function (el) {
      el.textContent = msg;
      el.dataset.state = options.state || '';
    });
    if (msg && !options.persist) {
      statusTimer = window.setTimeout(function () {
        setStatus('', { state: '' });
      }, options.timeoutMs || 2200);
    }
  }

  function setUpdateButtonsBusy(busy) {
    setButtonCollectionBusy(updateButtons, busy, {
      labelKey: 'threadUpdateOriginalLabel',
      busyLabel: updateButtons[0]
        ? (updateButtons[0].dataset.busyLabel || '[ Updating… ]')
        : '[ Updating… ]'
    });
  }

  function syncAutoUpdateToggles(checked) {
    toggleEls.forEach(function (el) {
      if (el.checked !== checked) el.checked = checked;
    });
  }

  function applyDeltaState(data) {
    if (data.reply_count !== undefined) {
      document.querySelectorAll('[data-role="thread-reply-count"]').forEach(function (el) {
        el.textContent = data.reply_count;
      });
    }
    var lockedEl = document.getElementById('thread-locked-indicator');
    if (lockedEl && data.locked !== undefined) lockedEl.style.display = data.locked ? '' : 'none';
    var stickyEl = document.getElementById('thread-sticky-indicator');
    if (stickyEl && data.sticky !== undefined) stickyEl.style.display = data.sticky ? '' : 'none';
  }

  function collectRefreshPostIds() {
    var ids = [];
    container.querySelectorAll('.post[data-media-processing-state="pending"]').forEach(function (postEl) {
      var id = parseInt((postEl.id || '').replace(/^p/, ''), 10);
      if (!isNaN(id) && ids.indexOf(id) === -1) ids.push(id);
    });
    return ids;
  }

  function applyRefreshedPosts(posts) {
    if (!Array.isArray(posts) || !posts.length) return false;
    var changed = false;
    posts.forEach(function (post) {
      if (!post || typeof post.id !== 'number' || typeof post.html !== 'string') return;
      var current = document.getElementById('p' + post.id);
      if (!current) return;
      var wrapper = document.createElement('div');
      wrapper.innerHTML = post.html;
      var replacement = wrapper.firstElementChild;
      if (!replacement) return;
      current.replaceWith(replacement);
      changed = true;
    });
    return changed;
  }

  window.fetchUpdates = function () {
    if (updating) return;
    updating = true;
    setUpdateButtonsBusy(true);
    setStatus('Updating\u2026', { state: 'working', persist: true });
    var url = '/' + board + '/thread/' + threadId + '/updates?since=' + lastId;
    var refreshIds = collectRefreshPostIds();
    if (refreshIds.length) {
      url += '&refresh=' + encodeURIComponent(refreshIds.join(','));
    }
    fetch(url)
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (data) {
        applyDeltaState(data);
        var refreshedChanged = applyRefreshedPosts(data.refreshed_posts);
        if (data.count > 0) {
          var frag = document.createElement('div');
          frag.innerHTML = data.html;
          while (frag.firstChild) container.appendChild(frag.firstChild);
          lastId = data.last_id;
          showPill(data.count);
        }
        if ((refreshedChanged || data.count > 0) && window._onNewPostsInserted) {
          window._onNewPostsInserted(container);
        }
        // Refresh nav bar if the board list changed since last poll.
        // boards_version is a monotonic counter incremented server-side
        // whenever a board is created, deleted, or restored.
        if (data.boards_version !== undefined && data.boards_version !== lastBoardsVersion) {
          lastBoardsVersion = data.boards_version;
          if (data.nav_html !== undefined) {
            var navEl = document.querySelector('nav.board-list');
            if (navEl) navEl.innerHTML = data.nav_html;
          }
        }
        setStatus(
          data.count > 0
            ? ('Updated. ' + data.count + ' new repl' + (data.count === 1 ? 'y.' : 'ies.'))
            : 'Updated.',
          { state: 'success' }
        );
        setUpdateButtonsBusy(false);
        updating = false;
      })
      .catch(function () {
        setStatus('Update failed.', { state: 'error', persist: true });
        setUpdateButtonsBusy(false);
        updating = false;
      });
  };

  function toggleAutoUpdate(cb) {
    autoOn = cb.checked;
    syncAutoUpdateToggles(autoOn);
    if (autoOn) {
      if (timer) clearInterval(timer);
      timer = setInterval(window.fetchUpdates, 15000);
      setStatus('Auto-update on.', { state: 'working' });
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
  var SCROLL_KEY = 'rustchan_reply_scroll_' + board + '_' + threadId;

  function saveReplyScrollPosition() {
    try {
      sessionStorage.setItem(
        SCROLL_KEY,
        JSON.stringify({
          path: window.location.pathname,
          x: window.pageXOffset || window.scrollX || 0,
          y: window.pageYOffset || window.scrollY || 0,
          ts: Date.now()
        })
      );
    } catch (e) {}
  }

  function restoreReplyScrollPosition() {
    var raw = null;
    try {
      raw = sessionStorage.getItem(SCROLL_KEY);
    } catch (e) {}
    if (!raw) return;

    var saved = null;
    try {
      saved = JSON.parse(raw);
    } catch (e) {}
    try {
      sessionStorage.removeItem(SCROLL_KEY);
    } catch (e) {}

    if (!saved || saved.path !== window.location.pathname) return;
    if (saved.ts && Date.now() - saved.ts > 2 * 60 * 1000) return;

    function restore() {
      window.scrollTo(saved.x || 0, saved.y || 0);
    }

    // Successful reply redirects include #p<id>; once we've recorded "(You)",
    // drop the fragment so the browser doesn't yank the viewport away again.
    if (/^#p\d+$/.test(window.location.hash) && window.history && window.history.replaceState) {
      window.history.replaceState({}, document.title, window.location.pathname + window.location.search);
    }

    restore();
    if (window.requestAnimationFrame) window.requestAnimationFrame(restore);
    window.setTimeout(restore, 0);
    window.addEventListener('load', restore, { once: true });
  }

  try {
    var pending = localStorage.getItem(PENDING_KEY);
    if (pending === '1') {
      localStorage.removeItem(PENDING_KEY);
      var hash = window.location.hash;
      var m = hash.match(/^#p(\d+)$/);
      if (m) {
        // Successful reply redirects land on #p<id>. Clear the saved composer
        // draft before other startup code strips the fragment for scroll restore.
        clearReplyDraftStorage();
        clearReplyDraftSubmitState();
        var newId = parseInt(m[1], 10);
        var existing = JSON.parse(localStorage.getItem(POSTS_KEY) || '[]');
        if (existing.indexOf(newId) === -1) existing.push(newId);
        localStorage.setItem(POSTS_KEY, JSON.stringify(existing));
      }
    }
  } catch (e) {}

  restoreReplyScrollPosition();

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
        saveReplyScrollPosition();
        try { localStorage.setItem(PENDING_KEY, '1'); } catch (e) {}
      });
    });
  }
  wireFormTracking();

  document.addEventListener('click', function (e) {
    if (e.target && e.target.classList.contains('post-toggle-btn')) {
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

  function highlightPostFromHash(scrollBehavior) {
    var match = window.location.hash.match(/^#p(\d+)$/);
    if (!match) {
      clearHighlight();
      return;
    }
    var target = document.getElementById('p' + match[1]);
    if (!target) return;
    highlightPost(match[1]);
    if (scrollBehavior && typeof target.scrollIntoView === 'function') {
      target.scrollIntoView({ behavior: scrollBehavior, block: 'start' });
    }
  }

  document.addEventListener('click', function (e) {
    if (e.target.classList.contains('quotelink')) return;
    if (e.target.classList.contains('backref')) return;
    clearHighlight();
  });

  document.addEventListener('DOMContentLoaded', function () {
    if (!/^#p\d+$/.test(window.location.hash)) return;
    if (window.requestAnimationFrame) {
      window.requestAnimationFrame(function () {
        highlightPostFromHash();
      });
    } else {
      highlightPostFromHash();
    }
  });

  window.addEventListener('hashchange', function () {
    if (!/^#p\d+$/.test(window.location.hash)) {
      clearHighlight();
      return;
    }
    var behavior = 'smooth';
    if (window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      behavior = 'auto';
    }
    highlightPostFromHash(behavior);
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

  // Show an inline "post not found" notice anchored to the clicked quotelink.
  // Reuses the existing hover popup element so the style is identical to a
  // real post preview — no new DOM structure needed.
  function showMissingPostPopup(link, pid) {
    clearTimeout(_hideTimer);
    popup.innerHTML =
      '<div class="missing-post-notice">' +
      '<span class="missing-post-icon">&#x2715;</span> ' +
      '<strong>&gt;&gt;' + pid + '</strong> — post not found' +
      '<span class="missing-post-sub">it may have been deleted</span>' +
      '</div>';
    popup.style.display = 'block';
    _popupTarget = null;
    positionPopup(link);
    // Auto-dismiss after 3 s so the user is not left with a stale tooltip.
    clearTimeout(_hideTimer);
    _hideTimer = setTimeout(hidePopup, 3000);
  }

  function wireQuotelinks(root) {
    root.querySelectorAll('a.quotelink[data-pid]').forEach(function (link) {
      if (link.dataset.quotelinkWired === '1') return;
      link.dataset.quotelinkWired = '1';
      var pid = link.getAttribute('data-pid');
      link.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); showPopup(link, pid); });
      link.addEventListener('mouseleave', function () { _hideTimer = setTimeout(hidePopup, 120); });
      link.addEventListener('click', function (e) {
        var target = document.getElementById('p' + pid);
        if (!target) {
          // Post is not on this page (deleted or in another thread).
          // Prevent navigation and show an inline error anchored to the link.
          e.preventDefault();
          e.stopPropagation();
          showMissingPostPopup(link, pid);
          return;
        }
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
      if (link.dataset.backrefWired === '1') return;
      link.dataset.backrefWired = '1';
      var pid = link.getAttribute('data-pid');
      link.addEventListener('mouseenter', function () { clearTimeout(_hideTimer); showPopup(link, pid); });
      link.addEventListener('mouseleave', function () { _hideTimer = setTimeout(hidePopup, 120); });
      link.addEventListener('click', function (e) {
        var target = document.getElementById('p' + pid);
        if (!target) {
          e.preventDefault();
          e.stopPropagation();
          showMissingPostPopup(link, pid);
          return;
        }
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
    document.querySelectorAll('#thread-posts .backrefs').forEach(function (span) {
      span.innerHTML = '';
    });
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
  var _cbHideTimer = null;

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
    var popup = getCbPopup();
    if (popup && popup.dataset.crosslinkPopupWired !== '1') {
      popup.dataset.crosslinkPopupWired = '1';
      popup.addEventListener('mouseenter', function () { clearTimeout(_cbHideTimer); });
      popup.addEventListener('mouseleave', function () {
        _cbHideTimer = setTimeout(function () { popup.style.display = 'none'; }, 120);
      });
    }

    root.querySelectorAll('a.crosslink[data-crossboard][data-pid]').forEach(function (link) {
      if (link.dataset.crosslinkWired === '1') return;
      link.dataset.crosslinkWired = '1';
      var board = link.getAttribute('data-crossboard');
      var pid = link.getAttribute('data-pid');
      if (!board || !pid) return;
      link.addEventListener('mouseenter', function () { clearTimeout(_cbHideTimer); fetchAndShow(link, board, pid); });
      link.addEventListener('mouseleave', function () {
        _cbHideTimer = setTimeout(function () { if (popup) popup.style.display = 'none'; }, 120);
      });
      link.addEventListener('click', function (e) {
        e.preventDefault();
        var key = board + ':' + pid;
        function navigate(threadId) {
          window.location.href = '/' + board + '/thread/' + threadId + '#p' + pid;
        }
        function showCbMissingError() {
          var cbPopup = getCbPopup();
          if (!cbPopup) return;
          cbPopup.innerHTML =
            '<div class="missing-post-notice">' +
            '<span class="missing-post-icon">&#x2715;</span> ' +
            '<strong>&gt;&gt;&gt;/' + board + '/' + pid + '</strong> — post not found' +
            '<span class="missing-post-sub">it may have been deleted</span>' +
            '</div>';
          cbPopup.style.display = 'block';
          positionCbPopup(link, cbPopup);
          setTimeout(function () { if (cbPopup) cbPopup.style.display = 'none'; }, 3000);
        }
        // If we already know the thread ID from a prior hover-preview fetch, navigate directly.
        if (_cbCache[key] && _cbCache[key].thread_id) { navigate(_cbCache[key].thread_id); return; }
        // If a prior fetch already confirmed the post is gone, show error inline.
        if (_cbCache[key] && !_cbCache[key].thread_id) { showCbMissingError(); return; }
        fetch('/api/post/' + board + '/' + pid)
          .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
          .then(function (data) {
            if (data.thread_id) {
              navigate(data.thread_id);
            } else {
              // API returned success but no thread_id — post is orphaned/deleted.
              showCbMissingError();
            }
          })
          .catch(function () {
            // 404 or network error — post is gone; show error inline.
            showCbMissingError();
          });
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

function clearBanDeletePreparation(form) {
  if (!form) return;
  form.dataset.banDeletePrepared = '';
  if (form.dataset.confirmSubmit && form.dataset.confirmSubmit.indexOf('Ban IP + delete post No.') === 0) {
    form.dataset.confirmSubmit = '';
  }
}

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
  form.dataset.banDeletePrepared = '1';
  form.dataset.confirmSubmit = 'Ban IP + delete post No.' + pid + '?';
  return true;
}

// ─── Poll management ──────────────────────────────────────────────────────────

function getPollOptionMaxLength(list) {
  if (!list) return 200;
  return parseInt(list.dataset.pollOptionMaxlength, 10) || 200;
}

function buildPollOptionRowHtml(count, maxLength) {
  return (
    '<input type="text" class="poll-option-input" name="poll_option" placeholder="Option ' + count + '" maxlength="' + maxLength + '">' +
    '<button type="button" class="poll-remove-btn" data-action="remove-poll-option" aria-label="Remove poll option" hidden>\u2715</button>'
  );
}

function addPollOption() {
  var list = document.getElementById('poll-options-list');
  if (!list) return;
  var count = list.querySelectorAll('.poll-option-row').length + 1;
  if (count > 10) return;
  var row = document.createElement('div');
  row.className = 'poll-option-row';
  row.innerHTML = buildPollOptionRowHtml(count, getPollOptionMaxLength(list));
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
    if (btn) btn.hidden = rows.length <= 2;
  });
}

// ─── Catalog sort ─────────────────────────────────────────────────────────────

function sortCatalog(mode) {
  try { sessionStorage.setItem('catalog_sort', mode); } catch (e) {}
  var grid = document.getElementById('catalog-grid');
  if (!grid) return;
  var items = Array.from(grid.querySelectorAll('.catalog-item'));
  items.sort(function (a, b) {
    var ap = parseInt(a.dataset.pinned) || 0;
    var bp = parseInt(b.dataset.pinned) || 0;
    if (ap !== bp) return bp - ap;
    var as_ = parseInt(a.dataset.sticky) || 0;
    var bs_ = parseInt(b.dataset.sticky) || 0;
    if (as_ !== bs_) return bs_ - as_;
    if (mode === 'bump') {
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

function setCatalogCommentVisibility(mode) {
  try { sessionStorage.setItem('catalog_show_comment', mode); } catch (e) {}
  var grid = document.getElementById('catalog-grid');
  if (!grid) return;
  grid.classList.toggle('catalog-comments-off', mode === 'off');
}

function togglePosterHighlights(threadId, posterId) {
  var posts = Array.from(document.querySelectorAll('.post[data-thread-id]'));
  var matching = posts.filter(function (post) {
    return post.dataset.threadId === String(threadId) && post.dataset.posterId === posterId;
  });
  if (!matching.length) return;

  var alreadyActive = matching.every(function (post) {
    return post.classList.contains('post-same-poster-highlighted');
  });

  posts.forEach(function (post) {
    post.classList.remove('post-same-poster-highlighted');
  });

  if (!alreadyActive) {
    matching.forEach(function (post) {
      post.classList.add('post-same-poster-highlighted');
    });
  }
}

// Restore saved catalog controls on page load
(function () {
  try {
    var sortValue = sessionStorage.getItem('catalog_sort') || 'bump';
    var sortSelect = document.getElementById('catalog-sort');
    if (sortSelect) {
      sortSelect.value = sortValue;
      sortCatalog(sortValue);
    }

    var showComment = sessionStorage.getItem('catalog_show_comment') || 'off';
    var commentSelect = document.getElementById('catalog-show-comment');
    if (commentSelect) {
      commentSelect.value = showComment;
      setCatalogCommentVisibility(showComment);
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
    if (statusEl) statusEl.textContent = 'solving proof-of-work…';
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
  if (
    e.target === document.getElementById('confirm-modal') ||
    e.target.id === 'confirm-modal-cancel'
  ) {
    e.preventDefault();
    closeConfirmModal(false);
    return;
  }
  if (e.target.id === 'confirm-modal-continue') {
    e.preventDefault();
    closeConfirmModal(true);
    return;
  }

  // data-action handlers
  var t = e.target.closest('[data-action]');
  if (t) {
    switch (t.dataset.action) {
      case 'toggle-post-form':
        e.preventDefault();
        togglePostForm();
        break;
      case 'open-post-form':
        e.preventDefault();
        clearRestoredAutoQuoteOnlyDraft();
        setPostFormOpen(true, { scrollIntoView: true });
        break;
      case 'dismiss-compress':    dismissCompressModal(); break;
      case 'start-compress':      startCompress(); break;
      case 'close-report':        closeReportModal(); break;
      case 'toggle-thread-menu':
        e.preventDefault();
        e.stopPropagation();
        toggleThreadMenu(t);
        break;
      case 'toggle-theme-picker':
        e.preventDefault();
        window.toggleThemePicker && window.toggleThemePicker();
        break;
      case 'set-theme':
        e.preventDefault();
        window.setTheme && window.setTheme(t.dataset.theme, t.getAttribute('href'));
        break;
      case 'remove-poll-option':  removePollOption(t); break;
      case 'add-poll-option':     addPollOption(); break;
      case 'append-reply':
        e.preventDefault();
        appendReply(t.dataset.id);
        break;
      case 'toggle-spoiler':
        t.classList.toggle('revealed');
        break;
      case 'expand-media':
        e.preventDefault();
        expandMedia(t);
        break;
      case 'collapse-media':      collapseMedia(t); break;
      case 'fetch-updates':       window.fetchUpdates && window.fetchUpdates(); break;
      case 'open-report':
        closeThreadMenus();
        openReportModal(t.dataset.pid, t.dataset.tid, t.dataset.board, t.dataset.csrf, t.dataset.reportLabel);
        break;
      case 'open-nsfw-disclaimer':
        e.preventDefault();
        openNsfwDisclaimer(t.dataset.returnTo, t.dataset.boardLabel);
        break;
      case 'close-nsfw-disclaimer':
        e.preventDefault();
        closeNsfwDisclaimer();
        break;
      case 'toggle-poster-highlight':
        e.preventDefault();
        togglePosterHighlights(t.dataset.threadId, t.dataset.posterId);
        break;
    }
  }

  if (!e.target.closest('.catalog-card-actions')) {
    closeThreadMenus();
  }

  // data-confirm: prompt before allowing click/submit
  var confirmEl = e.target.closest('[data-confirm]');
  if (confirmEl && confirmEl.dataset.rcConfirmBypass !== '1') {
    e.preventDefault();
    e.stopPropagation();
    requestConfirmation(confirmEl.dataset.confirm, confirmEl, {
      dangerous: isDangerousConfirmationTrigger(confirmEl, confirmEl.dataset.confirm)
    }).then(function (confirmed) {
      if (!confirmed) return;

      if (confirmEl.tagName === 'A' && confirmEl.href) {
        window.location.assign(confirmEl.href);
        return;
      }

      if (confirmEl.form && confirmEl.type === 'submit') {
        confirmEl.form.dataset.rcConfirmSubmitBypass = '1';
        requestFormSubmit(confirmEl.form, confirmEl);
        return;
      }

      confirmEl.dataset.rcConfirmBypass = '1';
      confirmEl.click();
    });
    return;
  }
  if (confirmEl && confirmEl.dataset.rcConfirmBypass === '1') {
    confirmEl.dataset.rcConfirmBypass = '';
  }
});

document.addEventListener('change', function (e) {
  var target = e.target;
  // File inputs: check size
  if (target.matches && target.matches('input[type="file"][data-onchange-check-size]')) {
    window.checkFileSize && window.checkFileSize(target);
  }
  // Autoupdate toggle
  if (target.matches && target.matches('[data-role="autoupdate-toggle"]')) {
    window._toggleAutoUpdate && window._toggleAutoUpdate(target);
  }
  // Catalog sort
  if (target.id === 'catalog-sort') {
    sortCatalog(target.value);
  }
  if (target.id === 'catalog-show-comment') {
    setCatalogCommentVisibility(target.value);
  }
  // Allow-editing checkbox: show/hide edit-window row
  if (target.name === 'allow_editing') {
    var row = target.closest('form') && target.closest('form').querySelector('.edit-window-row');
    if (row) row.style.display = target.checked ? '' : 'none';
  }
});

document.addEventListener('submit', function (e) {
  closeThreadMenus();
  var form = e.target;
  var submitter = e.submitter || null;
  if (form.matches && form.matches('form.post-form')) {
    if (captchaNonceMissing(form)) {
      e.preventDefault();
      showPostFormFeedback(
        form,
        'CAPTCHA is still solving. Wait for the checkmark before posting.'
      );
      setPostFormOpen(true, { scrollIntoView: true });
      return;
    }
    if (submitPostFormWithProgress(form)) {
      e.preventDefault();
      return;
    }
  }
  // data-ban-delete: admin ban+delete form
  if (form.dataset.banDeletePid && form.dataset.banDeletePrepared !== '1') {
    e.preventDefault();
    if (adminBanDelete(form, form.dataset.banDeletePid)) {
      requestFormSubmit(form, submitter);
    } else {
      clearBanDeletePreparation(form);
    }
    return;
  }
  // data-confirm-submit: prompt before form submission
  if (form.dataset.confirmSubmit && form.dataset.rcConfirmSubmitBypass !== '1') {
    e.preventDefault();
    requestConfirmation(form.dataset.confirmSubmit, submitter || form, {
      dangerous: isDangerousConfirmationTrigger(submitter || form, form.dataset.confirmSubmit)
    }).then(function (confirmed) {
      if (!confirmed) {
        if (form.dataset.banDeletePid) clearBanDeletePreparation(form);
        return;
      }
      form.dataset.rcConfirmSubmitBypass = '1';
      requestFormSubmit(form, submitter);
    });
    return;
  }
  if (form.dataset.rcConfirmSubmitBypass === '1') {
    form.dataset.rcConfirmSubmitBypass = '';
  }
  if (form.dataset.banDeletePrepared === '1') {
    clearBanDeletePreparation(form);
  }
});

document.addEventListener('keydown', function (e) {
  if (e.key === 'Escape') {
    if (ensureConfirmModal() && _confirmModal.style.display !== 'none') {
      e.preventDefault();
      closeConfirmModal(false);
      return;
    }
    closeThreadMenus();
  }
});

// ─── YouTube / Streamable embed unfurling ────────────────────────────────────
// FIX[YT-EMBED]: The previous approach placed buildEmbed() inside an inline
// <script> block in the Rust thread template.  Inline scripts are blocked by
// the page's CSP (`script-src 'self'` with no `'unsafe-inline'`), so thumbnails
// and inline playback were completely broken.
//
// The fix:
//   • The Rust template now emits a hidden <div id="thread-config"> element
//     carrying board-specific values as data-* attributes (embed-enabled,
//     draft-key).  No inline script is needed.
//   • buildEmbed() and the draft-autosave logic live here in main.js (loaded
//     via <script src="…" defer>, which the CSP allows).
//
// Supported YouTube URL formats handled by the Rust backend (sanitize.rs):
//   https://youtube.com/watch?v=VIDEOID
//   https://www.youtube.com/watch?v=VIDEOID
//   https://youtu.be/VIDEOID
//   https://youtube.com/shorts/VIDEOID
//   Any of the above with extra query params (&t=, &feature=, etc.)
//
// Thumbnail source : https://img.youtube.com/vi/VIDEOID/hqdefault.jpg
// Embed player     : https://www.youtube.com/embed/VIDEOID  (inline, no redirect)

(function () {
  var cfg = document.getElementById('thread-config');
  if (!cfg) return;                          // not a thread page
  if (cfg.dataset.embedEnabled !== '1') return; // embeds disabled for this board

  function buildEmbed(span) {
    var type = span.getAttribute('data-embed-type');
    var id   = span.getAttribute('data-embed-id');
    var url  = span.getAttribute('data-url') || span.textContent.trim();
    if (!type || !id) return;

    // Validate: only allow known embed types to prevent arbitrary iframe injection
    if (type !== 'youtube' && type !== 'streamable') return;

    // Validate YouTube ID format: 11 alphanumeric / dash / underscore chars
    if (type === 'youtube' && !/^[A-Za-z0-9_-]{11}$/.test(id)) return;
    if (type === 'streamable' && !/^[A-Za-z0-9_-]{1,16}$/.test(id)) return;

    // ── outer container: matches .file-container webm layout ─────────────
    var container = document.createElement('div');
    container.className = 'file-container video-embed-container';

    // ── file-info row (link + close button) ───────────────────────────────
    var info = document.createElement('div');
    info.className = 'file-info';
    var a = document.createElement('a');
    a.href = url; a.rel = 'nofollow noopener'; a.target = '_blank';
    a.textContent = url;
    var closeBtn = document.createElement('button');
    closeBtn.className = 'media-close-btn';
    closeBtn.innerHTML = '&#x2715; close';
    closeBtn.style.display = 'none';
    closeBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      collapseVideoEmbed(closeBtn);
    });
    info.appendChild(a);
    info.appendChild(closeBtn);
    container.appendChild(info);

    // ── thumbnail preview (styled like webm .media-preview) ───────────────
    var preview = document.createElement('div');
    preview.className = 'media-preview';
    preview.title = 'click to open embed';

    if (type === 'youtube') {
      var img = document.createElement('img');
      img.className = 'thumb';
      img.loading = 'lazy';
      img.alt = 'video thumbnail';
      // hqdefault (480×360) gives a larger, higher-quality thumbnail than
      // mqdefault (320×180) and is reliably available for all YouTube videos.
      img.src = 'https://img.youtube.com/vi/' + id + '/hqdefault.jpg';
      preview.appendChild(img);
    } else if (type === 'streamable') {
      var ph = document.createElement('div');
      ph.className = 'thumb embed-placeholder-thumb';
      ph.innerHTML = '&#9654; streamable';
      preview.appendChild(ph);
    }

    var overlay = document.createElement('div');
    overlay.className = 'media-expand-overlay';
    overlay.innerHTML = '&#9654;';
    preview.appendChild(overlay);

    preview.addEventListener('click', function () {
      expandVideoEmbed(preview, type, id, container);
    });
    container.appendChild(preview);

    // ── move container before the post-body; remove span from body text ───
    var postBody = span.closest('.post-body');
    if (postBody && postBody.parentNode) {
      span.remove();
      postBody.parentNode.insertBefore(container, postBody);
    } else {
      span.replaceWith(container);
    }
  }

  function applyEmbeds(root) {
    root.querySelectorAll('span.video-unfurl[data-embed-type]').forEach(buildEmbed);
  }

  applyEmbeds(document);

  // Wire into the thread auto-update hook so new replies also get embeds
  var _origEmbed = window._onNewPostsInserted;
  window._onNewPostsInserted = function (container) {
    if (_origEmbed) _origEmbed(container);
    applyEmbeds(container);
  };
})();

// ─── Draft autosave ───────────────────────────────────────────────────────────
// FIX[YT-EMBED]: Moved from inline <script> in thread.rs (was CSP-blocked) to
// here.  The draft key is now read from data-draft-key on #thread-config.

(function () {
  var cfg = document.getElementById('thread-config');
  if (!cfg) return;
  var DRAFT_KEY = cfg.dataset.draftKey;
  if (!DRAFT_KEY) return;
  var DRAFT_META_KEY = DRAFT_KEY + ':mode';

  var ta = getReplyBodyField();
  if (!ta) return;

  // If the last submit landed back on this thread with a post anchor, the
  // redirect was successful and any saved draft should be discarded before
  // restore runs.
  consumeSubmittedReplyDraft();

  // Restore saved draft on page load
  try {
    var saved = localStorage.getItem(DRAFT_KEY);
    var savedMode = localStorage.getItem(DRAFT_META_KEY);
    if (saved) {
      ta.value = saved;
      ta.dataset.draftRestored = '1';
      ta.dataset.lastPersistedDraft = saved;
      ta.dataset.lastPersistedDraftMode = savedMode || '';
      if (savedMode) {
        setReplyDraftMode(savedMode);
      } else if (isQuoteOnlyReplyDraft(saved)) {
        setReplyDraftMode('auto-quote-only');
      } else {
        setReplyDraftMode('manual');
      }
    }
  } catch (e) {}

  ta.addEventListener('input', function () {
    ta.dataset.draftRestored = '0';
    setReplyDraftSubmitting(false);
    clearReplyDraftSubmitState();
    setReplyDraftMode('manual');
    queueReplyDraftSave();
  });
  window.addEventListener('pagehide', flushReplyDraftStorage);

  // Persist the latest draft on submit, then pause autosave until the request
  // either redirects back successfully or the current page resumes editing.
  var form = ta.closest('form');
  if (form) {
    form.addEventListener('submit', function () {
      ta.dataset.draftRestored = '0';
      flushReplyDraftStorage();
      setReplyDraftSubmitting(true);
      markReplyDraftSubmitted();
    });

    form.addEventListener('rustchan:post-submit-reset', function () {
      setReplyDraftSubmitting(false);
      clearReplyDraftSubmitState();
      flushReplyDraftStorage();
    });
  }
})();

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

// ─── Rate-limit page redirect ────────────────────────────────────────────────
(function () {
  if (!document.body || document.body.dataset.rateLimitPage !== '1') return;
  setTimeout(function () {
    if (document.referrer) {
      window.location.href = document.referrer;
    } else {
      window.history.back();
    }
  }, 3000);
})();

// ─── Admin backup progress bar ────────────────────────────────────────────────
//
// Covers two flows:
//
//   A) "Save to server" forms — POST via fetch(), modal shows live progress,
//      "Done — reload" button appears when the fetch resolves.
//
//   B) "Download to computer" links — GET triggers a file download.  We show
//      the modal with live progress while the server builds the zip, then
//      dismiss it automatically once phase=DONE is reported.  The actual
//      download still happens natively in the browser (iframe trick).
//
// Note: all handlers here are CSP-safe (no inline onclick/onX attributes).
// The "Done — reload" button uses data-action="close-backup-modal" and is
// dispatched by the existing global click handler below.
//
// Phase codes (mirror middleware::backup_phase in Rust):
//   0=idle  1=snapshot_db  2=count_files  3=compress  4=save  5=done
(function () {
  var _pollTimer = null;
  var _downloadMode = false;  // true when modal is showing for a download

  var PHASE_LABELS = [
    'Idle',
    'Snapshotting database\u2026',
    'Counting files\u2026',
    'Compressing files\u2026',
    'Saving\u2026',
    'Done!',
  ];

  function showBackupModal(title) {
    var modal = document.getElementById('backup-modal');
    var titleEl = document.getElementById('backup-modal-title');
    var done = document.getElementById('backup-done-actions');
    if (!modal) return;
    if (titleEl) titleEl.textContent = title || '\uD83D\uDCBE Creating Backup\u2026';
    if (done) done.style.display = 'none';
    _setBkProgress(0, 'Starting\u2026');
    modal.style.display = 'flex';
  }

  function hideBackupModal() {
    var modal = document.getElementById('backup-modal');
    if (modal) modal.style.display = 'none';
  }

  function showDoneButton() {
    var done = document.getElementById('backup-done-actions');
    if (done) done.style.display = 'flex';
  }

  function _setBkProgress(pct, text) {
    var bar = document.getElementById('backup-progress-bar');
    var txt = document.getElementById('backup-progress-text');
    if (bar) bar.style.width = Math.min(100, Math.max(0, pct)) + '%';
    if (txt) txt.textContent = text;
  }

  function _startPolling(onDone) {
    if (_pollTimer) return;
    _pollTimer = setInterval(function () {
      fetch('/admin/backup/progress', { credentials: 'same-origin' })
        .then(function (r) { return r.json(); })
        .then(function (data) {
          var phase = data.phase || 0;
          var label = PHASE_LABELS[phase] || 'Working\u2026';
          var pct = 0;
          if (data.files_total > 0) {
            pct = Math.min(98, Math.round((data.files_done / data.files_total) * 100));
          } else if (phase === 1) { pct = 5; }
            else if (phase === 2) { pct = 10; }
          var detail = data.files_total > 0
            ? ' (' + data.files_done + '/' + data.files_total + ' files)'
            : '';
          _setBkProgress(pct, label + detail);

          // In download mode the fetch resolves as soon as the response headers
          // arrive (streaming body).  Poll phase instead to know when done.
          if (_downloadMode && phase === 5) {
            _stopPolling();
            _setBkProgress(100, '\u2713 Download ready!');
            // Auto-dismiss after 1.5 s — the file is already downloading.
            setTimeout(hideBackupModal, 1500);
            if (onDone) onDone();
          }
        })
        .catch(function () { /* ignore transient poll errors */ });
    }, 500);
  }

  function _stopPolling() {
    if (_pollTimer) { clearInterval(_pollTimer); _pollTimer = null; }
  }

  function _extractRefreshTarget(refreshValue) {
    if (!refreshValue) return '';
    var match = refreshValue.match(/url\s*=\s*(.+)$/i);
    if (!match) return '';
    return match[1].trim().replace(/^["']|["']$/g, '');
  }

  function _extractRedirectTargetFromHtml(html) {
    if (!html) return '';

    var metaMatch = html.match(/<meta[^>]+http-equiv=["']refresh["'][^>]+content=["'][^"']*url=([^"'>]+)["']/i);
    if (metaMatch && metaMatch[1]) return metaMatch[1].trim();

    var linkMatch = html.match(/<a[^>]+href=["']([^"']+)["'][^>]*>\s*Continue\s*<\/a>/i);
    if (linkMatch && linkMatch[1]) return linkMatch[1].trim();

    return '';
  }

  function _resolveRestoreRedirectTarget(xhr, form) {
    var payload = parseXhrJsonPayload(xhr);
    var explicitRedirect = xhr.getResponseHeader('X-Rustchan-Redirect') || '';
    if (explicitRedirect) return explicitRedirect;
    if (payload && payload.redirect_url) return payload.redirect_url;

    var refreshTarget = _extractRefreshTarget(xhr.getResponseHeader('refresh') || '');
    var htmlTarget = _extractRedirectTargetFromHtml(xhr.responseText || '');
    var responseUrl = absoluteUrl(xhr.responseURL || '');
    var formAction = absoluteUrl(form.action || '');
    var current = absoluteUrl(window.location.href);
    var target = absoluteUrl(refreshTarget || htmlTarget || '');

    if (target && target !== current) return target;
    if (responseUrl && responseUrl !== current && responseUrl !== formAction) return responseUrl;
    return '';
  }

  function _createBackupSubmitHelper(form, busyLabel) {
    return createAsyncSubmitHelper({
      form: form,
      busyLabel: busyLabel,
      labelKey: 'backupOriginalLabel',
      setProgress: function (percent, message) {
        _setBkProgress(percent, message);
      }
    });
  }

  function _submitRestoreUploadForm(form, title) {
    var xhr = null;
    var submitHelper = _createBackupSubmitHelper(form, 'Uploading…');
    _downloadMode = false;
    _stopPolling();
    showBackupModal(title);
    submitHelper.setProgress(0, 'Starting upload…');
    submitHelper.setBusy(true);

    xhr = new XMLHttpRequest();
    xhr.open((form.method || 'POST').toUpperCase(), form.action, true);
    xhr.withCredentials = true;
    xhr.setRequestHeader('X-Requested-With', 'XMLHttpRequest');

    xhr.upload.addEventListener('progress', function (event) {
      if (event.lengthComputable && event.total > 0) {
        var pct = Math.round((event.loaded / event.total) * 100);
        submitHelper.setProgress(
          pct,
          'Uploading restore file… ' +
          formatBytes(event.loaded) + ' / ' + formatBytes(event.total) +
          ' (' + pct + '%)'
        );
      } else {
        submitHelper.setProgress(15, 'Uploading restore file…');
      }
    });

    xhr.addEventListener('load', function () {
      submitHelper.setBusy(false);
      var payload = submitHelper.parsePayload(xhr);
      if (payload && payload.error) {
        submitHelper.setProgress(
          0,
          submitHelper.extractError(xhr, payload, 'Restore upload failed (' + xhr.status + ')')
        );
        showDoneButton();
        return;
      }
      if (xhr.status >= 200 && xhr.status < 400) {
        submitHelper.setProgress(100, 'Upload complete. Restoring backup…');
        var redirectTarget = _resolveRestoreRedirectTarget(xhr, form);
        if (redirectTarget) {
          window.location.assign(redirectTarget);
          return;
        }
        window.location.reload();
        return;
      }
      submitHelper.setProgress(
        0,
        submitHelper.extractError(xhr, payload, 'Restore upload failed (' + xhr.status + ')')
      );
      showDoneButton();
    });

    xhr.addEventListener('error', function () {
      submitHelper.setBusy(false);
      submitHelper.setProgress(0, 'Restore upload failed. Please try again.');
      showDoneButton();
    });

    xhr.addEventListener('abort', function () {
      submitHelper.setBusy(false);
      submitHelper.setProgress(0, 'Restore upload cancelled.');
      showDoneButton();
    });

    xhr.send(new FormData(form));
  }

  // ── Flow A: "Save to server" forms ──────────────────────────────────────────

  function _submitBackupForm(form, title, options) {
    options = options || {};
    var submitHelper = _createBackupSubmitHelper(
      form,
      options.downloadAfterCreate ? 'Preparing…' : 'Saving…'
    );
    _downloadMode = false;
    showBackupModal(title);
    _startPolling(null);
    submitHelper.setBusy(true);

    // URLSearchParams → application/x-www-form-urlencoded, required by Axum's Form<>.
    var params = new URLSearchParams(new FormData(form));
    var headers = {};
    if (options.downloadAfterCreate) {
      headers['X-Requested-With'] = 'XMLHttpRequest';
      headers['X-Rustchan-Download-After-Create'] = '1';
    }

    fetch(form.action, {
      method: 'POST',
      body: params,
      credentials: 'same-origin',
      headers: headers
    })
      .then(function (resp) {
        _stopPolling();
        if (!resp.ok && !resp.redirected) {
          submitHelper.setBusy(false);
          submitHelper.setProgress(0, 'Server returned an error (' + resp.status + ')');
          showDoneButton();
          return null;
        }

        var contentType = resp.headers.get('content-type') || '';
        if (contentType.indexOf('application/json') !== -1) {
          return resp.json();
        }

        submitHelper.setBusy(false);
        submitHelper.setProgress(100, '\u2713 Backup saved to server!');
        showDoneButton();
        return null;
      })
      .then(function (data) {
        if (!data) return;
        if (data.download_url) {
          submitHelper.setBusy(false);
          submitHelper.setProgress(100, '\u2713 Backup ready! Starting download\u2026');
          var a = document.createElement('a');
          a.href = data.download_url;
          a.download = '';
          a.style.display = 'none';
          document.body.appendChild(a);
          a.click();
          setTimeout(function () {
            if (a.parentNode) a.parentNode.removeChild(a);
            hideBackupModal();
          }, 1500);
          return;
        }
        submitHelper.setBusy(false);
        submitHelper.setProgress(100, '\u2713 Backup saved to server!');
        showDoneButton();
      })
      .catch(function (err) {
        _stopPolling();
        submitHelper.setBusy(false);
        submitHelper.setProgress(0, 'Error: ' + (err.message || 'backup failed'));
        showDoneButton();
      });
  }

  // ── Flow B: "Download to computer" links ────────────────────────────────────

  function _triggerDownload(url, label) {
    _downloadMode = true;
    showBackupModal('\uD83D\uDCBE Preparing ' + (label || 'backup') + '\u2026');
    _startPolling(null);

    // Trigger the file download without navigating away.
    // A hidden <a download>.click() makes a standard GET request and the
    // browser saves the response as a file — no page navigation occurs.
    // We cannot use an <iframe> here because the page's CSP frame-src policy
    // only permits YouTube and Streamable origins, not 'self', so an iframe
    // pointing at /admin/backup/... would be silently blocked and the GET
    // would never fire (leaving the progress bar stuck on "Idle").
    var a = document.createElement('a');
    a.href = url;
    a.download = '';
    a.style.display = 'none';
    document.body.appendChild(a);
    a.click();
    setTimeout(function () {
      if (a.parentNode) a.parentNode.removeChild(a);
    }, 5000);
  }

  // ── Wiring ───────────────────────────────────────────────────────────────────

  document.addEventListener('DOMContentLoaded', function () {
    // Flow A — full-site "save to server"
    var fullForm = document.getElementById('full-backup-create-form');
    if (fullForm) {
      fullForm.addEventListener('submit', function (e) {
        e.preventDefault();
        _submitBackupForm(fullForm, '\uD83D\uDCBE Creating Full Backup\u2026');
      });
    }

    // Flow A — per-board "save to server"
    document.querySelectorAll('.board-backup-create-form').forEach(function (form) {
      form.addEventListener('submit', function (e) {
        e.preventDefault();
        var board = form.dataset.board || '';
        _submitBackupForm(form, '\uD83D\uDCBE Backing up /' + board + '/\u2026');
      });
    });

    document.querySelectorAll('.board-backup-download-form').forEach(function (form) {
      form.addEventListener('submit', function (e) {
        e.preventDefault();
        var board = form.dataset.board || '';
        _submitBackupForm(
          form,
          '\uD83D\uDCBE Preparing /' + board + '/ download\u2026',
          { downloadAfterCreate: true }
        );
      });
    });

    // Flow B — all "download to computer" links
    document.querySelectorAll('a.backup-download-link').forEach(function (link) {
      link.addEventListener('click', function (e) {
        e.preventDefault();
        var label = link.dataset.backupLabel || 'backup';
        _triggerDownload(link.href, label);
      });
    });

    document.querySelectorAll('form.backup-restore-upload-form').forEach(function (form) {
      form.addEventListener('submit', function (e) {
        e.preventDefault();
        var label = form.dataset.restoreLabel || 'backup';
        _submitRestoreUploadForm(form, '\u21bb Uploading ' + label + '…');
      });
    });
  });

  // ── "Done — reload" button (CSP-safe, no inline onclick) ────────────────────
  // Registered here rather than in the global data-action handler so it lives
  // in the same closure and can call hideBackupModal() + reload.
  document.addEventListener('click', function (e) {
    if (e.target.closest('[data-action="close-backup-modal"]')) {
      hideBackupModal();
      window.location.reload();
    }
  });
})();
