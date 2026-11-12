(function () {
  'use strict';

  var root = document.documentElement;
  var toggle = document.getElementById('theme-toggle');
  var lang = root.lang === 'en' ? 'en' : 'id';

  function setTheme(theme) {
    root.dataset.theme = theme;
    try {
      localStorage.setItem('workamon-theme', theme);
    } catch (error) {}
  }

  function isDark() {
    return root.dataset.theme === 'dark';
  }

  if (toggle) {
    toggle.addEventListener('click', function () {
      setTheme(isDark() ? 'light' : 'dark');
    });
  }

  document.addEventListener('keydown', function (event) {
    if (!event.shiftKey || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key !== 'D' && event.key !== 'd') return;
    var target = event.target;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable)) return;
    setTheme(isDark() ? 'light' : 'dark');
  });

  document.querySelectorAll('[data-lang-switch]').forEach(function (link) {
    link.addEventListener('click', function (event) {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.button !== 0) return;
      event.preventDefault();
      var next = window.location.pathname + window.location.search + window.location.hash;
      var target = link.getAttribute('data-lang-switch');
      window.location.href = '/language/' + encodeURIComponent(target) + '?next=' + encodeURIComponent(next);
    });
  });

  document.querySelectorAll('form[data-confirm]').forEach(function (form) {
    form.addEventListener('submit', function (event) {
      var fallback = lang === 'id' ? 'Lanjutkan?' : 'Continue?';
      if (!window.confirm(form.getAttribute('data-confirm') || fallback)) {
        event.preventDefault();
      }
    });
  });

  document.querySelectorAll('[data-dismiss]').forEach(function (button) {
    button.addEventListener('click', function () {
      var flash = button.closest('.flash');
      if (flash) flash.remove();
    });
  });

  function setText(key, value) {
    if (value === undefined || value === null) return;
    document.querySelectorAll('[data-live="' + key + '"]').forEach(function (element) {
      element.textContent = value;
    });
  }

  function setBar(key, value) {
    var percent = Math.max(0, Math.min(100, Number(value) || 0));
    document.querySelectorAll('[data-bar="' + key + '"]').forEach(function (element) {
      element.style.width = percent + '%';
    });
  }

  function setDocker(online) {
    document.querySelectorAll('[data-docker-dot]').forEach(function (dot) {
      dot.classList.toggle('dot-ok', !!online);
      dot.classList.toggle('dot-off', !online);
    });
  }

  async function refreshLive() {
    if (document.hidden) return;
    try {
      var response = await fetch('/api/live', {
        headers: { Accept: 'application/json' },
        cache: 'no-store'
      });
      if (!response.ok) return;
      var data = await response.json();
      ['cpu_text', 'mem_text', 'mem_pct_text', 'disk_text', 'disk_pct_text', 'uptime_text',
        'containers_running', 'containers_stopped', 'containers_unhealthy', 'docker_status'
      ].forEach(function (key) {
        setText(key, data[key]);
      });
      setBar('cpu', data.cpu_pct);
      setBar('mem', data.mem_pct);
      setBar('disk', data.disk_pct);
      setDocker(data.docker_ok);
      document.querySelectorAll('[data-live-ts]').forEach(function (element) {
        element.textContent = data.ts_text || '';
      });
    } catch (error) {}
  }

  if (document.querySelectorAll('[data-live]').length > 0) {
    refreshLive();
    window.setInterval(refreshLive, 5000);
  }

  async function refreshContainer(id) {
    if (document.hidden) return;
    try {
      var response = await fetch('/api/containers/' + encodeURIComponent(id) + '/stats', {
        headers: { Accept: 'application/json' },
        cache: 'no-store'
      });
      if (!response.ok) return;
      var data = await response.json();
      ['cpu', 'mem', 'mem_pct', 'net'].forEach(function (key) {
        var value = data[key];
        if (value === undefined) return;
        document.querySelectorAll('[data-cstats="' + key + '"]').forEach(function (element) {
          element.textContent = value;
        });
      });
      var cpu = Math.max(0, Math.min(100, Number(data.cpu_pct) || 0));
      var mem = Math.max(0, Math.min(100, Number(data.mem_pct_value) || 0));
      document.querySelectorAll('[data-cbar="cpu"]').forEach(function (element) {
        element.style.width = cpu + '%';
      });
      document.querySelectorAll('[data-cbar="mem"]').forEach(function (element) {
        element.style.width = mem + '%';
      });
    } catch (error) {}
  }

  var detail = document.querySelector('[data-container-stats]');
  if (detail) {
    var containerId = detail.getAttribute('data-container-stats');
    refreshContainer(containerId);
    window.setInterval(function () {
      refreshContainer(containerId);
    }, 5000);
  }

  function clamp(value, min, max) {
    return Math.max(min, Math.min(max, value));
  }

  function niceMax(value) {
    if (!isFinite(value) || value <= 0) return 1;
    var magnitude = Math.pow(10, Math.floor(Math.log10(value)));
    var normalized = value / magnitude;
    var nice = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 2.5 ? 2.5 : normalized <= 5 ? 5 : 10;
    return nice * magnitude;
  }

  function clockLabel(ts, wide) {
    var date = new Date(ts * 1000);
    try {
      if (wide) {
        return date.toLocaleDateString(lang === 'id' ? 'id-ID' : 'en-US', { day: '2-digit', month: 'short' });
      }
      return date.toLocaleTimeString(lang === 'id' ? 'id-ID' : 'en-US', { hour: '2-digit', minute: '2-digit', hour12: false });
    } catch (error) {
      return '';
    }
  }

  function svgText(x, y, anchor, text, className) {
    return '<text' + (className ? ' class="' + className + '"' : '') + ' x="' + x.toFixed(1) + '" y="' + y.toFixed(2) + '" text-anchor="' + anchor + '">' + text + '</text>';
  }

  function buildChart(points, key, unit) {
    var data = points.map(function (point) {
      return { ts: Number(point.ts), value: Number(point[key]) };
    }).filter(function (point) {
      return isFinite(point.value) && point.value !== null;
    });

    if (!data.length) {
      return '<div class="chart-empty">' + (lang === 'id' ? 'Belum ada sampel pada rentang ini.' : 'No samples in this range.') + '</div>';
    }

    var width = 1000;
    var height = 190;
    var padLeft = 46;
    var padRight = 12;
    var padTop = 14;
    var padBottom = 22;
    var plotW = width - padLeft - padRight;
    var plotH = height - padTop - padBottom;
    var t0 = data[0].ts;
    var t1 = data[data.length - 1].ts;
    if (t1 <= t0) t1 = t0 + 60;

    var minValue = data.reduce(function (min, point) { return Math.min(min, point.value); }, Infinity);
    var maxValue = data.reduce(function (max, point) { return Math.max(max, point.value); }, -Infinity);
    var lo = unit === 'percent' ? 0 : Math.min(0, minValue);
    var hi = unit === 'percent' ? 100 : Math.max(niceMax(maxValue), 1);
    if (hi <= lo) hi = lo + 1;

    function x(ts) { return padLeft + ((ts - t0) / (t1 - t0)) * plotW; }
    function y(value) { return padTop + (1 - (value - lo) / (hi - lo)) * plotH; }

    function label(value) {
      if (unit === 'percent') return Math.round(value) + '%';
      return String(Math.round(value));
    }

    var line = '';
    data.forEach(function (point, index) {
      line += (index === 0 ? 'M' : 'L') + x(point.ts).toFixed(2) + ' ' + y(point.value).toFixed(2) + ' ';
    });

    var area = 'M' + x(t0).toFixed(2) + ' ' + (padTop + plotH).toFixed(2) + ' L' + x(data[0].ts).toFixed(2) + ' ' + y(data[0].value).toFixed(2) + ' ';
    data.slice(1).forEach(function (point) {
      area += 'L' + x(point.ts).toFixed(2) + ' ' + y(point.value).toFixed(2) + ' ';
    });
    area += 'L' + x(t1).toFixed(2) + ' ' + (padTop + plotH).toFixed(2) + ' Z';

    var grid = '';
    var ticks = 4;
    for (var index = 0; index <= ticks; index++) {
      var ratio = index / ticks;
      var value = lo + (hi - lo) * ratio;
      var yPos = y(value);
      grid += '<line x1="' + padLeft.toFixed(1) + '" y1="' + yPos.toFixed(2) + '" x2="' + (width - padRight).toFixed(1) + '" y2="' + yPos.toFixed(2) + '"/>';
      grid += svgText(padLeft - 8, yPos, 'end', label(value));
    }

    var midTs = t0 + (t1 - t0) / 2;
    var wide = (t1 - t0) > 86400;
    var last = data[data.length - 1];
    var classes = { cpu: 'chart-cpu', mem: 'chart-mem', disk: 'chart-disk', containers: 'chart-count' };

    return '<svg class="chart ' + (classes[key] || 'chart-cpu') + '" viewBox="0 0 ' + width + ' ' + height + '" role="img" aria-label="chart">' +
      '<g class="chart-grid">' + grid + '</g>' +
      '<path class="chart-area" d="' + area + '"/>' +
      '<path class="chart-line" d="' + line + '"/>' +
      '<circle class="chart-dot" cx="' + x(last.ts).toFixed(2) + '" cy="' + y(last.value).toFixed(2) + '" r="2.6"/>' +
      svgText(padLeft, height - 6, 'start', clockLabel(t0, wide)) +
      svgText(padLeft + plotW / 2, height - 6, 'middle', clockLabel(midTs, wide)) +
      svgText(width - padRight, height - 6, 'end', clockLabel(t1, wide)) +
      svgText(width - padRight, padTop + 4, 'end', label(last.value), 'chart-last') +
      '</svg>';
  }

  var liveCharts = Array.prototype.slice.call(document.querySelectorAll('[data-live-chart]'));
  if (liveCharts.length > 0) {
    var liveWindow = Number(liveCharts[0].getAttribute('data-live-window')) || 1800;

    var refreshCharts = async function () {
      if (document.hidden) return;
      try {
        var response = await fetch('/api/metrics/live?window=' + liveWindow, {
          headers: { Accept: 'application/json' },
          cache: 'no-store'
        });
        if (!response.ok) return;
        var data = await response.json();
        liveCharts.forEach(function (element) {
          var key = element.getAttribute('data-live-chart');
          var unit = element.getAttribute('data-live-unit') || 'percent';
          element.innerHTML = buildChart(data.points || [], key, unit);
        });
        if (data.summary) {
          Object.keys(data.summary).forEach(function (key) {
            var value = data.summary[key];
            document.querySelectorAll('[data-live-summary="' + key + '"]').forEach(function (element) {
              element.textContent = value;
            });
          });
        }
      } catch (error) {}
    };

    refreshCharts();
    window.setInterval(refreshCharts, 5000);
  }

  document.querySelectorAll('time[data-ts]').forEach(function (element) {
    var ts = Number(element.getAttribute('data-ts'));
    if (ts > 0) {
      element.title = new Date(ts * 1000).toLocaleString(lang === 'id' ? 'id-ID' : 'en-US');
    }
  });
})();
