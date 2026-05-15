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

  function firstChildDetails(node) {
    if (!node || !node.children) return null;
    for (var i = 0; i < node.children.length; i += 1) {
      var child = node.children[i];
      if (child.tagName && child.tagName.toLowerCase() === 'details') {
        return child;
      }
      if (child.tagName && child.tagName.toLowerCase() === 'section') {
        var nested = firstChildDetails(child);
        if (nested) return nested;
      }
    }
    return null;
  }

  function openHashTargetDetails() {
    if (!window.location.hash || window.location.hash.length < 2) return;
    var id = decodeURIComponent(window.location.hash.slice(1));
    if (!id) return;
    var target = document.getElementById(id);
    if (!target) return;
    openDetailsAncestors(target);
    var dropdown = firstChildDetails(target);
    if (dropdown) dropdown.open = true;
    if (typeof target.scrollIntoView === 'function') {
      target.scrollIntoView({ block: 'start' });
    }
  }

  document.addEventListener('DOMContentLoaded', openHashTargetDetails);
  window.addEventListener('hashchange', openHashTargetDetails);
  openHashTargetDetails();
})();

(function () {
  function openAdminSection(sectionId) {
    if (!sectionId) return;
    var section = document.getElementById(sectionId);
    if (!section) return;
    var details = section.querySelector('details.admin-dropdown');
    if (details) details.open = true;
    if (typeof section.scrollIntoView === 'function') {
      section.scrollIntoView({ block: 'start' });
    }
  }

  function initAdminSectionLinks() {
    document.querySelectorAll('[data-open-admin-section]').forEach(function (link) {
      link.addEventListener('click', function () {
        openAdminSection(link.getAttribute('data-open-admin-section'));
      });
    });
  }

  function initDiagnosticsDialog() {
    document.querySelectorAll('[data-admin-diagnostics]').forEach(function (details) {
      var summary = details.querySelector('summary');
      var closeButton = details.querySelector('[data-admin-diagnostics-close]');
      var copyButton = details.querySelector('[data-admin-diagnostics-copy]');
      var text = details.querySelector('[data-admin-diagnostics-text]');

      if (summary) {
        summary.addEventListener('click', function (event) {
          event.preventDefault();
          details.open = true;
          if (copyButton && typeof copyButton.focus === 'function') {
            copyButton.focus();
          }
        });
      }

      if (closeButton) {
        closeButton.addEventListener('click', function () {
          details.open = false;
          if (summary && typeof summary.focus === 'function') {
            summary.focus();
          }
        });
      }

      if (copyButton && text) {
        copyButton.addEventListener('click', function () {
          var value = text.textContent || '';
          if (navigator.clipboard && navigator.clipboard.writeText) {
            navigator.clipboard.writeText(value).then(function () {
              copyButton.textContent = 'copied';
              window.setTimeout(function () {
                copyButton.textContent = 'copy';
              }, 1500);
            }).catch(function () {
              copyButton.textContent = 'copy failed';
            });
          }
        });
      }

      document.addEventListener('keydown', function (event) {
        if (event.key === 'Escape' && details.open) {
          details.open = false;
        }
      });
    });
  }

  document.addEventListener('DOMContentLoaded', function () {
    initAdminSectionLinks();
    initDiagnosticsDialog();
  });
})();

(function () {
  var PRESET_CONFIGS = {
    forest: {
      background_color: '#141914',
      panel_color: '#1e281d',
      card_color: '#243022',
      op_card_color: '#2a3827',
      text_color: '#e5e6d8',
      muted_text_color: '#b0b796',
      link_color: '#7ab84e',
      link_hover_color: '#a8d77b',
      border_color: '#4c6441',
      input_background_color: '#161d15',
      input_text_color: '#eceedd',
      input_border_color: '#657e57',
      button_background_color: '#466735',
      button_text_color: '#f4f5e8',
      button_border_color: '#6d9652',
      button_hover_color: '#577f42',
      header_background_color: '#1b2419',
      header_text_color: '#f0efdd',
      header_border_color: '#6a8c4f',
      quote_color: '#98c86e',
      meta_text_color: '#c2c6ab',
      success_color: '#7eb25b',
      danger_color: '#c46f6f',
      border_radius_px: '8',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    },
    'blue-sky': {
      background_color: '#dfeaf2',
      panel_color: '#f8fbfe',
      card_color: '#f3f7fb',
      op_card_color: '#edf4fa',
      text_color: '#223446',
      muted_text_color: '#61758b',
      link_color: '#356d9b',
      link_hover_color: '#204f7a',
      border_color: '#bdd1e3',
      input_background_color: '#ffffff',
      input_text_color: '#223446',
      input_border_color: '#9fb8cc',
      button_background_color: '#5d8fb5',
      button_text_color: '#f8fcff',
      button_border_color: '#4d7696',
      button_hover_color: '#476f92',
      header_background_color: '#edf5fb',
      header_text_color: '#1f3344',
      header_border_color: '#9eb8ce',
      quote_color: '#4f7f4e',
      meta_text_color: '#61758b',
      success_color: '#4c8a67',
      danger_color: '#b85d69',
      border_radius_px: '10',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    },
    'deep-orbit': {
      background_color: '#161b26',
      panel_color: '#202636',
      card_color: '#252d40',
      op_card_color: '#2a3347',
      text_color: '#dde3ef',
      muted_text_color: '#99a5ba',
      link_color: '#8dc6cd',
      link_hover_color: '#badbe5',
      border_color: '#3d485f',
      input_background_color: '#171d2a',
      input_text_color: '#dde3ef',
      input_border_color: '#53617d',
      button_background_color: '#64739d',
      button_text_color: '#f4f7fb',
      button_border_color: '#54607f',
      button_hover_color: '#7381ab',
      header_background_color: '#1b2130',
      header_text_color: '#eef3fb',
      header_border_color: '#56637e',
      quote_color: '#9fcb97',
      meta_text_color: '#aab6cb',
      success_color: '#6eb090',
      danger_color: '#c87d8f',
      border_radius_px: '12',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    },
    terminal: {
      background_color: '#050505',
      panel_color: '#0f1210',
      card_color: '#101612',
      op_card_color: '#121a14',
      text_color: '#c7e7c7',
      muted_text_color: '#89ae89',
      link_color: '#26d85c',
      link_hover_color: '#cffff0',
      border_color: '#224228',
      input_background_color: '#060c06',
      input_text_color: '#c7e7c7',
      input_border_color: '#1f4a27',
      button_background_color: '#103c1d',
      button_text_color: '#d9f7dd',
      button_border_color: '#2d7a44',
      button_hover_color: '#17552a',
      header_background_color: '#0f1210',
      header_text_color: '#d4f0d4',
      header_border_color: '#17b84a',
      quote_color: '#8fd66d',
      meta_text_color: '#8fbd93',
      success_color: '#26d85c',
      danger_color: '#ff4c68',
      border_radius_px: '0',
      density: 'compact',
      font_family: 'system_mono',
      advanced_css: ''
    },
    dorfic: {
      background_color: '#17110b',
      panel_color: '#2a1d11',
      card_color: '#332215',
      op_card_color: '#3a2718',
      text_color: '#ecd5a8',
      muted_text_color: '#b6965f',
      link_color: '#d9a755',
      link_hover_color: '#ffcc66',
      border_color: '#694726',
      input_background_color: '#20150d',
      input_text_color: '#f0ddb5',
      input_border_color: '#7d5530',
      button_background_color: '#5b3818',
      button_text_color: '#ffe1aa',
      button_border_color: '#8c602f',
      button_hover_color: '#714821',
      header_background_color: '#26190f',
      header_text_color: '#f6e3bd',
      header_border_color: '#a1682d',
      quote_color: '#d3b46b',
      meta_text_color: '#c3a06f',
      success_color: '#d3a04a',
      danger_color: '#d97d5d',
      border_radius_px: '0',
      density: 'compact',
      font_family: 'system_mono',
      advanced_css: ''
    },
    chanclassic: {
      background_color: '#eef2ff',
      panel_color: '#ffffff',
      card_color: '#f7f8ff',
      op_card_color: '#f4f4fb',
      text_color: '#1c1c2b',
      muted_text_color: '#62627a',
      link_color: '#8b0000',
      link_hover_color: '#b20000',
      border_color: '#c4c9df',
      input_background_color: '#ffffff',
      input_text_color: '#1f1f30',
      input_border_color: '#acb4d0',
      button_background_color: '#e8e9f7',
      button_text_color: '#2c2b44',
      button_border_color: '#b1b6cb',
      button_hover_color: '#d9dbeb',
      header_background_color: '#d8daf0',
      header_text_color: '#24243a',
      header_border_color: '#aab2d3',
      quote_color: '#789922',
      meta_text_color: '#62627a',
      success_color: '#6d8e24',
      danger_color: '#b54747',
      border_radius_px: '3',
      density: 'compact',
      font_family: 'system_serif',
      advanced_css: ''
    },
    aero: {
      background_color: '#d9eef8',
      panel_color: '#ffffff',
      card_color: '#f8fdff',
      op_card_color: '#eef8fd',
      text_color: '#234156',
      muted_text_color: '#5f7e93',
      link_color: '#1a6fa8',
      link_hover_color: '#0d5a8a',
      border_color: '#a3c8de',
      input_background_color: '#ffffff',
      input_text_color: '#234156',
      input_border_color: '#94b7cc',
      button_background_color: '#dceefb',
      button_text_color: '#20435b',
      button_border_color: '#8eb5d0',
      button_hover_color: '#cfe6f7',
      header_background_color: '#f4fbff',
      header_text_color: '#21465f',
      header_border_color: '#8eb7d5',
      quote_color: '#4a8f59',
      meta_text_color: '#64849b',
      success_color: '#4a9f7a',
      danger_color: '#c76272',
      border_radius_px: '12',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    },
    neoncubicle: {
      background_color: '#17141b',
      panel_color: '#241f2b',
      card_color: '#2c2431',
      op_card_color: '#32283a',
      text_color: '#efe6ef',
      muted_text_color: '#ac96a9',
      link_color: '#db63b4',
      link_hover_color: '#ff9fdc',
      border_color: '#5f4a63',
      input_background_color: '#1a151f',
      input_text_color: '#f6eef7',
      input_border_color: '#6e5470',
      button_background_color: '#55314b',
      button_text_color: '#ffeefe',
      button_border_color: '#8a4e78',
      button_hover_color: '#683d5b',
      header_background_color: '#211b27',
      header_text_color: '#f7eef7',
      header_border_color: '#985787',
      quote_color: '#a4d283',
      meta_text_color: '#bb9fb4',
      success_color: '#72bb8c',
      danger_color: '#d97b9a',
      border_radius_px: '8',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    },
    fluorogrid: {
      background_color: '#f4f6fb',
      panel_color: '#ffffff',
      card_color: '#fefefe',
      op_card_color: '#f9f7ff',
      text_color: '#1f2430',
      muted_text_color: '#5f6473',
      link_color: '#7a38aa',
      link_hover_color: '#4b9bc1',
      border_color: '#cfd4ea',
      input_background_color: '#ffffff',
      input_text_color: '#1f2430',
      input_border_color: '#b9bfd9',
      button_background_color: '#f0ebff',
      button_text_color: '#31205a',
      button_border_color: '#b898df',
      button_hover_color: '#e6dcff',
      header_background_color: '#ffffff',
      header_text_color: '#2d2a46',
      header_border_color: '#a5afda',
      quote_color: '#2b9e66',
      meta_text_color: '#6d7280',
      success_color: '#27a26b',
      danger_color: '#d05f79',
      border_radius_px: '0',
      density: 'cozy',
      font_family: 'system_sans',
      advanced_css: ''
    }
  };

  function cssFont(fontFamily) {
    if (fontFamily === 'system_serif') {
      return "Georgia, 'Times New Roman', Times, 'Noto Serif', serif";
    }
    if (fontFamily === 'system_mono') {
      return "'SFMono-Regular', Consolas, 'Liberation Mono', 'Courier New', monospace";
    }
    return "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif";
  }

  function fieldValue(form, name) {
    var input = form.querySelector('[name="' + name + '"]');
    return input ? input.value : '';
  }

  function setFieldValue(form, name, value) {
    var input = form.querySelector('[name="' + name + '"]');
    if (!input) return;
    input.value = value;
    updateFieldMirrors(form, input);
  }

  function isHexColor(value) {
    return /^#[0-9a-fA-F]{6}$/.test(value || '');
  }

  function updateFieldMirrors(form, input) {
    if (!input || !input.name) return;
    var colorMirror = form.querySelector('[data-theme-builder-value-for="' + input.name + '"]');
    if (colorMirror) colorMirror.textContent = input.value;
    var colorPicker = form.querySelector('[data-theme-builder-color-for="' + input.name + '"]');
    if (colorPicker && isHexColor(input.value)) colorPicker.value = input.value;
    var rangeMirror = form.querySelector('[data-theme-builder-range-value="' + input.name + '"]');
    if (rangeMirror) rangeMirror.textContent = input.value + 'px';
  }

  function syncColorPickerField(form, picker) {
    var fieldName = picker.getAttribute('data-theme-builder-color-for');
    if (!fieldName) return null;
    var input = form.querySelector('[name="' + fieldName + '"]');
    if (!input) return null;
    input.value = picker.value;
    updateFieldMirrors(form, input);
    return input;
  }

  function applyPreset(form, presetName) {
    var preset = PRESET_CONFIGS[presetName];
    if (!preset) return;
    Object.keys(preset).forEach(function (key) {
      setFieldValue(form, key, preset[key]);
    });
  }

  function previewCss(form, selector) {
    var font = cssFont(fieldValue(form, 'font_family'));
    var gap = fieldValue(form, 'density') === 'compact' ? '0.35rem' : '0.55rem';
    var pad = fieldValue(form, 'density') === 'compact' ? '0.45rem' : '0.75rem';
    var radius = fieldValue(form, 'border_radius_px') || '8';
    return (
      selector + ' {' +
      '--theme-preview-bg:' + fieldValue(form, 'background_color') + ';' +
      '--theme-preview-panel:' + fieldValue(form, 'panel_color') + ';' +
      '--theme-preview-card:' + fieldValue(form, 'card_color') + ';' +
      '--theme-preview-op:' + fieldValue(form, 'op_card_color') + ';' +
      '--theme-preview-text:' + fieldValue(form, 'text_color') + ';' +
      '--theme-preview-muted:' + fieldValue(form, 'muted_text_color') + ';' +
      '--theme-preview-link:' + fieldValue(form, 'link_color') + ';' +
      '--theme-preview-link-hover:' + fieldValue(form, 'link_hover_color') + ';' +
      '--theme-preview-border:' + fieldValue(form, 'border_color') + ';' +
      '--theme-preview-input-bg:' + fieldValue(form, 'input_background_color') + ';' +
      '--theme-preview-input-text:' + fieldValue(form, 'input_text_color') + ';' +
      '--theme-preview-input-border:' + fieldValue(form, 'input_border_color') + ';' +
      '--theme-preview-button-bg:' + fieldValue(form, 'button_background_color') + ';' +
      '--theme-preview-button-text:' + fieldValue(form, 'button_text_color') + ';' +
      '--theme-preview-button-border:' + fieldValue(form, 'button_border_color') + ';' +
      '--theme-preview-button-hover:' + fieldValue(form, 'button_hover_color') + ';' +
      '--theme-preview-header-bg:' + fieldValue(form, 'header_background_color') + ';' +
      '--theme-preview-header-text:' + fieldValue(form, 'header_text_color') + ';' +
      '--theme-preview-header-border:' + fieldValue(form, 'header_border_color') + ';' +
      '--theme-preview-quote:' + fieldValue(form, 'quote_color') + ';' +
      '--theme-preview-radius:' + radius + 'px;' +
      '--theme-preview-gap:' + gap + ';' +
      '--theme-preview-pad:' + pad + ';' +
      '--theme-preview-font:' + font + ';' +
      '} ' +
      selector + ' .admin-flash.flash-ok { border-color:' + fieldValue(form, 'success_color') + '; color:' + fieldValue(form, 'success_color') + '; } ' +
      selector + ' .admin-flash.flash-error { border-color:' + fieldValue(form, 'danger_color') + '; color:' + fieldValue(form, 'danger_color') + '; }'
    );
  }

  function syncPreview(form) {
    var styleNode = form.querySelector('[data-theme-preview-style]');
    var preview = form.querySelector('[data-theme-preview-slug]');
    if (!styleNode || !preview) return;
    var selector = '[data-theme-preview-slug="' + preview.getAttribute('data-theme-preview-slug') + '"]';
    styleNode.textContent = previewCss(form, selector);
  }

  function initThemeBuilders(root) {
    (root || document).querySelectorAll('[data-theme-builder]').forEach(function (builder) {
      if (builder.dataset.themeBuilderReady === '1') return;
      builder.dataset.themeBuilderReady = '1';
      builder.querySelectorAll('[data-theme-builder-field]').forEach(function (input) {
        updateFieldMirrors(builder, input);
      });
      syncPreview(builder);
      builder.addEventListener('input', function (event) {
        var target = event.target;
        if (!target) return;
        if (target.hasAttribute('data-theme-builder-color-for')) {
          target = syncColorPickerField(builder, target);
        }
        if (!target || !target.name) return;
        updateFieldMirrors(builder, target);
        syncPreview(builder);
      });
      builder.addEventListener('change', function (event) {
        var target = event.target;
        if (!target) return;
        if (target.hasAttribute('data-theme-builder-color-for')) {
          target = syncColorPickerField(builder, target);
        }
        if (!target || !target.name) return;
        if (target.name === 'base_preset') {
          applyPreset(builder, target.value);
        }
        updateFieldMirrors(builder, target);
        syncPreview(builder);
      });
    });
  }

  document.addEventListener('DOMContentLoaded', function () {
    initThemeBuilders(document);
  });
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
    var status = document.getElementById('admin-live-log-status');
    var fileLabel = document.getElementById('admin-live-log-file');
    var refreshBtn = document.getElementById('admin-live-log-refresh');
    var clearBtn = document.getElementById('admin-live-log-clear');
    var autoscroll = document.getElementById('admin-live-log-autoscroll');
    if (!output) return;

    var timer = null;
    var lastText = '';
    var clearedBaseline = '';
    var clearedFile = '';
    var requestInFlight = false;
    var requestSerial = 0;
    var consecutiveFailures = 0;
    var pollIntervalMs = 2000;
    var requestTimeoutMs = 8000;
    var maxPollIntervalMs = 15000;

    function setStatus(message) {
      if (status) status.textContent = message;
    }

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

    function scheduleNextPoll(delayMs) {
      if (timer) clearTimeout(timer);
      timer = window.setTimeout(fetchLog, delayMs);
    }

    function retryDelayMs() {
      if (consecutiveFailures <= 0) {
        return pollIntervalMs;
      }
      return Math.min(
        pollIntervalMs * Math.pow(2, Math.min(consecutiveFailures - 1, 3)),
        maxPollIntervalMs
      );
    }

    function fetchLog(force) {
      if (requestInFlight && !force) return;
      requestInFlight = true;
      requestSerial += 1;
      var serial = requestSerial;
      var controller = typeof AbortController === 'function' ? new AbortController() : null;
      var timeoutHandle = window.setTimeout(function () {
        if (controller) {
          controller.abort();
        }
      }, requestTimeoutMs);

      if (!lastText) {
        setStatus('Connecting to live log…');
      }

      fetch('/admin/log/live?bytes=65536', {
        credentials: 'same-origin',
        cache: 'no-store',
        signal: controller ? controller.signal : undefined
      })
        .then(function (resp) { return resp.json(); })
        .then(function (data) {
          if (serial !== requestSerial) return;
          window.clearTimeout(timeoutHandle);
          requestInFlight = false;
          consecutiveFailures = 0;
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
          setStatus('Live log connected. Retrying automatically if this connection stalls.');
          if (fullText !== lastText) {
            lastText = fullText;
            var text = visibleText(fullText, fileName);
            output.textContent = text || 'Waiting for new log lines\u2026';
          }
          if (!autoscroll || autoscroll.checked) {
            output.scrollTop = output.scrollHeight;
          }
          scheduleNextPoll(pollIntervalMs);
        })
        .catch(function (error) {
          if (serial !== requestSerial) return;
          window.clearTimeout(timeoutHandle);
          requestInFlight = false;
          consecutiveFailures += 1;
          var delayMs = retryDelayMs();
          var timedOut = !!(error && error.name === 'AbortError');
          if (!lastText) {
            output.textContent = timedOut
              ? 'Live log request timed out. Retrying\u2026'
              : 'Unable to load live log. Retrying\u2026';
          }
          setStatus(
            timedOut
              ? 'Live log request timed out over this connection. Retrying in ' + Math.round(delayMs / 1000) + 's.'
              : 'Live log unavailable right now. Retrying in ' + Math.round(delayMs / 1000) + 's.'
          );
          scheduleNextPoll(delayMs);
        });
    }

    if (refreshBtn) {
      refreshBtn.addEventListener('click', function () {
        consecutiveFailures = 0;
        if (timer) clearTimeout(timer);
        fetchLog(true);
      });
    }

    if (clearBtn) {
      clearBtn.addEventListener('click', function () {
        clearedBaseline = lastText;
        clearedFile = fileLabel ? fileLabel.textContent : '';
        output.textContent = 'Waiting for new log lines\u2026';
      });
    }

    fetchLog(false);
  }

  document.addEventListener('DOMContentLoaded', initAdminLiveLog);
})();

(function () {
  function fetchJsonWithTimeout(url, timeoutMs) {
    if (!window.AbortController && window.XMLHttpRequest) {
      return new Promise(function (resolve, reject) {
        var xhr = new XMLHttpRequest();
        xhr.open('GET', url, true);
        xhr.withCredentials = true;
        xhr.timeout = timeoutMs;
        xhr.setRequestHeader('Accept', 'application/json');
        xhr.addEventListener('load', function () {
          if (xhr.status < 200 || xhr.status >= 300) {
            reject(new Error('poll request failed'));
            return;
          }
          try {
            resolve(JSON.parse(xhr.responseText || '{}'));
          } catch (error) {
            reject(error);
          }
        });
        xhr.addEventListener('timeout', function () {
          var error = new Error('poll request timed out');
          error.name = 'AbortError';
          reject(error);
        });
        xhr.addEventListener('error', function () {
          reject(new Error('poll request failed'));
        });
        xhr.send();
      });
    }

    var controller = null;
    var timer = null;
    var options = {
      credentials: 'same-origin',
      headers: { 'Accept': 'application/json' },
      cache: 'no-store'
    };

    if (window.AbortController) {
      controller = new AbortController();
      options.signal = controller.signal;
      timer = window.setTimeout(function () {
        controller.abort();
      }, timeoutMs);
    }

    return fetch(url, options)
      .then(function (resp) {
        if (timer) window.clearTimeout(timer);
        if (!resp.ok) throw new Error('poll request failed');
        return resp.json();
      }, function (error) {
        if (timer) window.clearTimeout(timer);
        throw error;
      });
  }

  function initSiteHealthJobPolling() {
    var container = document.querySelector('[data-admin-health-jobs-url]');
    if (!container) return;

    var url = container.getAttribute('data-admin-health-jobs-url');
    if (!url) return;

    var fieldNames = [
      'running_jobs',
      'queued_jobs',
      'recent_completed_jobs',
      'failed_jobs',
      'backup_jobs',
      'restore_jobs'
    ];
    var fields = {};
    fieldNames.forEach(function (name) {
      fields[name] = container.querySelector('[data-admin-health-job="' + name + '"]');
    });
    var details = container.querySelector('[data-admin-health-job-details]');
    var panels = {
      failed: container.querySelector('[data-admin-health-job-panel="failed"]'),
      completed: container.querySelector('[data-admin-health-job-panel="completed"]')
    };
    var lists = {
      failed: container.querySelector('[data-admin-health-job-list="failed"]'),
      completed: container.querySelector('[data-admin-health-job-list="completed"]')
    };

    function appendJobMeta(row, label, value) {
      var item = document.createElement('span');
      item.appendChild(document.createTextNode(label + ': '));
      var strong = document.createElement('strong');
      strong.textContent = value == null || value === '' ? 'n/a' : String(value);
      item.appendChild(strong);
      row.appendChild(item);
    }

    function renderJobList(name, jobs) {
      var list = lists[name];
      if (!list) return;
      list.textContent = '';
      if (!Array.isArray(jobs) || jobs.length === 0) {
        var empty = document.createElement('p');
        empty.className = 'admin-copy';
        empty.textContent = 'No recent jobs recorded.';
        list.appendChild(empty);
        return;
      }
      jobs.forEach(function (job) {
        var card = document.createElement('article');
        card.className = 'admin-health-job-card';
        var title = document.createElement('h4');
        title.textContent = job.name || job.type || 'Background job';
        card.appendChild(title);
        var meta = document.createElement('div');
        meta.className = 'admin-health-job-meta';
        appendJobMeta(meta, 'id', job.id);
        appendJobMeta(meta, 'type', job.type);
        appendJobMeta(meta, 'status', job.status);
        appendJobMeta(meta, 'attempts', job.attempts);
        appendJobMeta(meta, 'updated', job.updated_at);
        card.appendChild(meta);
        if (job.error) {
          var error = document.createElement('p');
          error.className = 'admin-health-job-error';
          error.textContent = job.error;
          card.appendChild(error);
        }
        list.appendChild(card);
      });
    }

    function applyJobs(data) {
      fieldNames.forEach(function (name) {
        if (!fields[name] || data[name] === undefined || data[name] === null) return;
        fields[name].textContent = String(data[name]);
      });
      renderJobList('failed', data.recent_failed_job_details);
      renderJobList('completed', data.recent_completed_job_details);
    }

    container.querySelectorAll('[data-admin-health-toggle]').forEach(function (button) {
      button.addEventListener('click', function () {
        var target = button.getAttribute('data-admin-health-toggle');
        if (!details || !panels[target]) return;
        var isOpen = !panels[target].hidden;
        Object.keys(panels).forEach(function (name) {
          if (panels[name]) panels[name].hidden = true;
        });
        panels[target].hidden = isOpen;
        details.hidden = isOpen;
      });
    });

    function poll() {
      fetchJsonWithTimeout(url, 8000).then(applyJobs, function () {
        // Keep the last known values visible; the next poll will retry.
      });
    }

    poll();
    window.setInterval(poll, 5000);
  }

  document.addEventListener('DOMContentLoaded', initSiteHealthJobPolling);

  function requestHeaders(headers) {
    var pairs = [];
    Object.keys(headers || {}).forEach(function (key) {
      pairs.push([key, headers[key]]);
    });
    return pairs;
  }

  function xhrResponse(xhr) {
    return {
      ok: xhr.status >= 200 && xhr.status < 300,
      status: xhr.status,
      redirected: false,
      headers: {
        get: function (name) {
          return xhr.getResponseHeader(name);
        }
      },
      json: function () {
        try {
          return Promise.resolve(JSON.parse(xhr.responseText || '{}'));
        } catch (error) {
          return Promise.reject(error);
        }
      }
    };
  }

  function xhrRequestWithTimeout(url, options, timeoutMs) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      var body = options.body || null;
      xhr.open(options.method || 'GET', url, true);
      xhr.withCredentials = options.credentials !== 'omit';
      xhr.timeout = timeoutMs;
      requestHeaders(options.headers).forEach(function (pair) {
        xhr.setRequestHeader(pair[0], pair[1]);
      });
      if (typeof URLSearchParams !== 'undefined' && body instanceof URLSearchParams) {
        xhr.setRequestHeader('Content-Type', 'application/x-www-form-urlencoded;charset=UTF-8');
        body = body.toString();
      }
      xhr.addEventListener('load', function () {
        resolve(xhrResponse(xhr));
      });
      xhr.addEventListener('timeout', function () {
        var error = new Error('request timed out');
        error.name = 'AbortError';
        reject(error);
      });
      xhr.addEventListener('error', function () {
        reject(new Error('request failed'));
      });
      xhr.send(body);
    });
  }

  function requestWithTimeout(url, options, timeoutMs) {
    options = options || {};
    timeoutMs = timeoutMs || 600000;
    if (!window.AbortController && window.XMLHttpRequest) {
      return xhrRequestWithTimeout(url, options, timeoutMs);
    }
    if (!window.fetch) return Promise.reject(new Error('request unsupported'));

    var timer = null;
    var controller = null;
    if (window.AbortController) {
      controller = new AbortController();
      options.signal = controller.signal;
      timer = window.setTimeout(function () {
        controller.abort();
      }, timeoutMs);
    }

    return fetch(url, options).then(function (response) {
      if (timer) window.clearTimeout(timer);
      return response;
    }, function (error) {
      if (timer) window.clearTimeout(timer);
      throw error;
    });
  }

  function createAdminJsonPoller(options) {
    options = options || {};
    var stopped = false;
    var inFlight = false;
    var timer = null;
    var failures = 0;
    var baseDelay = options.baseDelayMs || 750;
    var timeoutMs = options.timeoutMs || 20000;
    var maxDelay = options.maxDelayMs || 12000;

    function nextDelay(failed) {
      if (!failed) return baseDelay;
      return Math.min(maxDelay, baseDelay * Math.pow(2, Math.min(failures - 1, 4)));
    }

    function schedule(delayMs) {
      if (stopped) return;
      timer = window.setTimeout(poll, delayMs);
    }

    function poll() {
      if (stopped || inFlight) return;
      inFlight = true;
      fetchJsonWithTimeout(options.url, timeoutMs)
        .then(function (data) {
          failures = 0;
          if (options.onData && options.onData(data) === false) {
            stopped = true;
            return;
          }
          schedule(nextDelay(false));
        })
        .catch(function (error) {
          failures += 1;
          var delay = nextDelay(true);
          if (options.onStatus) {
            options.onStatus(
              error && error.name === 'AbortError'
                ? 'Progress request timed out. Retrying in ' + Math.round(delay / 1000) + 's...'
                : 'Progress unavailable. Retrying in ' + Math.round(delay / 1000) + 's...'
            );
          }
          schedule(delay);
        })
        .then(function () {
          inFlight = false;
        }, function () {
          inFlight = false;
        });
    }

    return {
      start: function () {
        stopped = false;
        if (timer) window.clearTimeout(timer);
        poll();
      },
      stop: function () {
        stopped = true;
        if (timer) {
          window.clearTimeout(timer);
          timer = null;
        }
      }
    };
  }

  window.createAdminJsonPoller = createAdminJsonPoller;
  window.adminRequestWithTimeout = requestWithTimeout;
})();

(function () {
  function initDbRepairProgress() {
    var wrap = document.querySelector('[data-db-repair-progress]');
    if (!wrap) return;

    var bar = wrap.querySelector('[data-db-repair-progress-bar]');
    var text = wrap.querySelector('[data-db-repair-progress-text]');
    var progressUrl = wrap.getAttribute('data-db-repair-progress-url') || '/admin/db/repair/progress';
    var redirected = false;
    var poller = null;

    function setProgress(percent, message) {
      percent = Math.min(100, Math.max(0, Number(percent) || 0));
      if (bar) bar.style.width = percent + '%';
      if (text) text.textContent = message || 'Working...';
    }

    function finish(data) {
      if (poller) poller.stop();
      if (redirected) return;
      redirected = true;
      setProgress(data && data.percent, data && data.label);
      window.setTimeout(function () {
        window.location.assign((data && data.redirect_url) || '/admin/db/repair/status');
      }, 700);
    }

    poller = window.createAdminJsonPoller({
      url: progressUrl,
      baseDelayMs: 750,
      timeoutMs: 20000,
      onData: function (data) {
        setProgress(data.percent, data.label);
        if (data.done) {
          finish(data);
          return false;
        }
        return true;
      },
      onStatus: function (message) {
        setProgress(5, 'Still working. ' + message);
      }
    });
    poller.start();
  }

  document.addEventListener('DOMContentLoaded', initDbRepairProgress);
})();

// Backup progress modal for both POST-based saves and GET downloads.
// Handlers stay CSP-safe and reuse the same phase codes as middleware::backup_phase.
(function () {
  var _poller = null;
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
    if (_poller) return;
    _poller = window.createAdminJsonPoller({
      url: '/admin/backup/progress',
      baseDelayMs: 700,
      timeoutMs: 20000,
      onData: function (data) {
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
          return false;
        }
        if (phase === 5) {
          _stopPolling();
          _setBkProgress(100, 'Backup completed. Waiting for final response\u2026');
          return false;
        }
        return true;
      },
      onStatus: function (message) {
        _setBkProgress(5, 'Still working. ' + message);
      }
    });
    _poller.start();
  }

  function _stopPolling() {
    if (_poller) {
      _poller.stop();
      _poller = null;
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
    xhr.timeout = 600000;
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
          submitHelper.extractError(xhr, payload, title + ' failed (' + xhr.status + ')')
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
        submitHelper.extractError(xhr, payload, title + ' failed (' + xhr.status + ')')
      );
      showDoneButton();
    });

    xhr.addEventListener('error', function () {
      submitHelper.setBusy(false);
      submitHelper.setProgress(0, title + ' failed. Request may still have succeeded. Refresh before retrying.');
      showDoneButton();
    });

    xhr.addEventListener('timeout', function () {
      submitHelper.setBusy(false);
      submitHelper.setProgress(0, title + ' timed out. Request may still have succeeded. Refresh before retrying.');
      showDoneButton();
    });

    xhr.addEventListener('abort', function () {
      submitHelper.setBusy(false);
      submitHelper.setProgress(0, title + ' cancelled.');
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

    window.adminRequestWithTimeout(form.action, {
      method: 'POST',
      body: params,
      credentials: 'same-origin',
      headers: headers
    }, 600000)
      .then(function (resp) {
        _stopPolling();
        if (!resp.ok && !resp.redirected) {
          submitHelper.setBusy(false);
          submitHelper.setProgress(0, title + ' failed (' + resp.status + ').');
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
        submitHelper.setProgress(
          0,
          title + ': ' + (err.message || 'request failed') +
          '. The request may still have succeeded. Refresh before retrying.'
        );
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
