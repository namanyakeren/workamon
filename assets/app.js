(function () {
  'use strict';

  var root = document.documentElement;
  var toggle = document.getElementById('theme-toggle');

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

  document.querySelectorAll('form[data-confirm]').forEach(function (form) {
    form.addEventListener('submit', function (event) {
      if (!window.confirm(form.getAttribute('data-confirm') || 'Lanjutkan?')) {
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
    window.setInterval(refreshLive, 10000);
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

  document.querySelectorAll('time[data-ts]').forEach(function (element) {
    var ts = Number(element.getAttribute('data-ts'));
    if (ts > 0) {
      element.title = new Date(ts * 1000).toLocaleString();
    }
  });
})();
