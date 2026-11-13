(function () {
  'use strict';

  function ready(fn) {
    if (document.readyState === 'loading') {
      document.addEventListener('DOMContentLoaded', fn);
    } else {
      fn();
    }
  }

  ready(function () {
    var host = document.getElementById('terminal');
    if (!host) return;

    var status = document.getElementById('terminal-status');
    var dot = document.getElementById('terminal-dot');
    var reconnect = document.getElementById('terminal-reconnect');
    var clear = document.getElementById('terminal-clear');
    var hostOk = host.getAttribute('data-host-ok') === 'true';
    var isEn = document.documentElement.lang === 'en';
    var text = {
      xtermFailed: isEn ? 'Failed to load xterm.js.' : 'xterm.js gagal dimuat.',
      disabled: isEn ? 'Host terminal is disabled.' : 'Host terminal dimatikan.',
      disabledHint: isEn
        ? 'Set WORKAMON_HOST_EXEC=1 and run the container with pid: host + privileged.'
        : 'Set WORKAMON_HOST_EXEC=1 dan jalankan container dengan pid: host + privileged.',
      connecting: isEn ? 'Connecting…' : 'Menghubungkan…',
      connected: isEn ? 'Connected' : 'Terhubung',
      disconnected: isEn ? 'Disconnected' : 'Terputus',
      error: isEn ? 'Connection error' : 'Kesalahan koneksi'
    };

    if (!window.Terminal) {
      host.textContent = text.xtermFailed;
      return;
    }

    var term = new window.Terminal({
      cursorBlink: true,
      cursorStyle: 'bar',
      fontFamily: '"JetBrains Mono", "SFMono-Regular", Consolas, monospace',
      fontSize: 13,
      lineHeight: 1.35,
      scrollback: 5000,
      allowTransparency: false,
      theme: {
        background: '#0b1220',
        foreground: '#dbe7f3',
        cursor: '#67e8f9',
        selectionBackground: '#1e4b63',
        black: '#0b1220',
        red: '#fb7185',
        green: '#86efac',
        yellow: '#fde68a',
        blue: '#93c5fd',
        magenta: '#c4b5fd',
        cyan: '#67e8f9',
        white: '#e2e8f0',
        brightBlack: '#64748b',
        brightRed: '#fda4af',
        brightGreen: '#bbf7d0',
        brightYellow: '#fef08a',
        brightBlue: '#bfdbfe',
        brightMagenta: '#ddd6fe',
        brightCyan: '#a5f3fc',
        brightWhite: '#f8fafc'
      }
    });
    var fit = null;
    if (window.FitAddon && window.FitAddon.FitAddon) {
      fit = new window.FitAddon.FitAddon();
      term.loadAddon(fit);
    }
    term.open(host);

    function fitNow() {
      if (!fit) return;
      try {
        fit.fit();
      } catch (error) {}
    }

    var ws = null;
    var retryTimer = null;
    var socketSeq = 0;

    function setStatus(text, ok) {
      if (status) status.textContent = text;
      if (dot) {
        dot.classList.toggle('dot-ok', !!ok);
        dot.classList.toggle('dot-off', !ok);
      }
    }

    function send(payload) {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(payload));
      }
    }

    function connect() {
      if (!hostOk) {
        setStatus(text.disabled, false);
        term.writeln('\x1b[33m' + text.disabled + '\x1b[0m');
        term.writeln(text.disabledHint);
        return;
      }
      socketSeq += 1;
      var seq = socketSeq;
      if (ws) {
        try { ws.close(); } catch (error) {}
        ws = null;
      }
      if (retryTimer) {
        window.clearTimeout(retryTimer);
        retryTimer = null;
      }
      setStatus(text.connecting, false);
      var scheme = window.location.protocol === 'https:' ? 'wss' : 'ws';
      ws = new WebSocket(scheme + '://' + window.location.host + '/ws/terminal');
      ws.binaryType = 'arraybuffer';

      ws.onopen = function () {
        setStatus(text.connected, true);
        fitNow();
        send({ type: 'resize', cols: term.cols, rows: term.rows });
        term.focus();
      };
      ws.onmessage = function (event) {
        if (typeof event.data === 'string') {
          term.write(event.data);
        } else {
          term.write(new Uint8Array(event.data));
        }
      };
      ws.onerror = function () {
        setStatus(text.error, false);
      };
      ws.onclose = function () {
        if (seq !== socketSeq) return;
        setStatus(text.disconnected, false);
        retryTimer = window.setTimeout(connect, 2500);
      };
    }

    term.onData(function (data) {
      send({ type: 'input', data: data });
    });
    if (term.onBinary) {
      term.onBinary(function (data) {
        send({ type: 'input', data: data });
      });
    }
    term.onResize(function (size) {
      send({ type: 'resize', cols: size.cols, rows: size.rows });
    });

    if (clear) {
      clear.addEventListener('click', function () {
        term.clear();
        term.focus();
      });
    }
    if (reconnect) {
      reconnect.addEventListener('click', function () {
        term.reset();
        connect();
      });
    }

    var resizeTimer = null;
    window.addEventListener('resize', function () {
      if (resizeTimer) window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(fitNow, 120);
    });

    var themeObserver = new MutationObserver(function () {
      // xterm memakai tema gelap tetap agar kontras terminal stabil.
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

    window.requestAnimationFrame(function () {
      fitNow();
      connect();
    });
  });
})();
