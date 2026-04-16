// admin.js - RustChan admin-panel-only client-side logic
'use strict';

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
      var hasExplicitOpenState = details.hasAttribute('open');

      if (!hasExplicitOpenState) {
        var stored = readAdminDropdownState(key);
        if (stored === '1') {
          details.open = true;
        } else if (stored === '0') {
          details.open = false;
        }
      }

      details.addEventListener('toggle', function () {
        writeAdminDropdownState(key, details.open);
      });
    });
  }

  document.addEventListener('DOMContentLoaded', initPersistentAdminDropdowns);
})();

(function () {
  function openDetailsAncestors(node) {
    var current = node;
    while (current) {
      if (current.tagName && current.tagName.toLowerCase() === 'details') {
        current.open = true;
      }
      current = current.parentElement;
    }
  }

  function openHashTargetDetails() {
    if (!window.location.hash || window.location.hash.length < 2) return;
    var id = decodeURIComponent(window.location.hash.slice(1));
    if (!id) return;
    var target = document.getElementById(id);
    if (!target) return;
    openDetailsAncestors(target);
    if (typeof target.scrollIntoView === 'function') {
      target.scrollIntoView({ block: 'start' });
    }
  }

  document.addEventListener('DOMContentLoaded', openHashTargetDetails);
  window.addEventListener('hashchange', openHashTargetDetails);
  openHashTargetDetails();
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
          output.textContent = text || 'Waiting for new log lines\u2026';
          if (!autoscroll || autoscroll.checked) {
            output.scrollTop = output.scrollHeight;
          }
        })
        .catch(function () {
          output.textContent = 'Unable to load live log.';
        });
    }

    function startPolling() {
      if (timer) clearInterval(timer);
      timer = setInterval(fetchLog, 2000);
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
        output.textContent = 'Waiting for new log lines\u2026';
      });
    }

    fetchLog();
    startPolling();
  }

  document.addEventListener('DOMContentLoaded', initAdminLiveLog);
})();

// Backup progress modal for both POST-based saves and GET downloads.
// Handlers stay CSP-safe and reuse the same phase codes as middleware::backup_phase.
(function () {
  var _pollTimer = null;
  var _downloadMode = false;

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
          } else if (phase === 1) {
            pct = 5;
          } else if (phase === 2) {
            pct = 10;
          }
          var detail = data.files_total > 0
            ? ' (' + data.files_done + '/' + data.files_total + ' files)'
            : '';
          _setBkProgress(pct, label + detail);

          if (_downloadMode && phase === 5) {
            _stopPolling();
            _setBkProgress(100, '\u2713 Download ready!');
            setTimeout(hideBackupModal, 1500);
            if (onDone) onDone();
          }
        })
        .catch(function () {});
    }, 500);
  }

  function _stopPolling() {
    if (_pollTimer) {
      clearInterval(_pollTimer);
      _pollTimer = null;
    }
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
    var submitHelper = _createBackupSubmitHelper(form, 'Uploading\u2026');
    _downloadMode = false;
    _stopPolling();
    showBackupModal(title);
    submitHelper.setProgress(0, 'Starting upload\u2026');
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
          'Uploading restore file\u2026 ' +
          formatBytes(event.loaded) + ' / ' + formatBytes(event.total) +
          ' (' + pct + '%)'
        );
      } else {
        submitHelper.setProgress(15, 'Uploading restore file\u2026');
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
        submitHelper.setProgress(100, 'Upload complete. Restoring backup\u2026');
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

  function _submitBackupForm(form, title, options) {
    options = options || {};
    var submitHelper = _createBackupSubmitHelper(
      form,
      options.downloadAfterCreate ? 'Preparing\u2026' : 'Saving\u2026'
    );
    _downloadMode = false;
    showBackupModal(title);
    _startPolling(null);
    submitHelper.setBusy(true);

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

  function _triggerDownload(url, label) {
    _downloadMode = true;
    showBackupModal('\uD83D\uDCBE Preparing ' + (label || 'backup') + '\u2026');
    _startPolling(null);

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

  document.addEventListener('DOMContentLoaded', function () {
    var fullForm = document.getElementById('full-backup-create-form');
    if (fullForm) {
      fullForm.addEventListener('submit', function (e) {
        e.preventDefault();
        _submitBackupForm(fullForm, '\uD83D\uDCBE Creating Full Backup\u2026');
      });
    }

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
        _submitRestoreUploadForm(form, '\u21BB Uploading ' + label + '\u2026');
      });
    });
  });

  document.addEventListener('click', function (e) {
    if (e.target.closest('[data-action="close-backup-modal"]')) {
      hideBackupModal();
      window.location.reload();
    }
  });
})();
