// Polyphony Website — Scroll Animations & Interactions

(function () {
  'use strict';

  // --- Theme switching ---
  var themeCycle = ['light', 'system', 'dark'];
  var savedTheme = localStorage.getItem('theme') || 'system';

  function applyTheme(mode) {
    if (mode === 'dark' || (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
      document.documentElement.classList.add('dark');
    } else {
      document.documentElement.classList.remove('dark');
    }
  }

  function updateThemeIcon(mode) {
    var light = document.getElementById('theme-icon-light');
    var system = document.getElementById('theme-icon-system');
    var dark = document.getElementById('theme-icon-dark');
    var btn = document.getElementById('theme-toggle');
    if (light) light.classList.toggle('hidden', mode !== 'light');
    if (system) system.classList.toggle('hidden', mode !== 'system');
    if (dark) dark.classList.toggle('hidden', mode !== 'dark');
    var titles = { light: 'Theme: Light', system: 'Theme: System', dark: 'Theme: Dark' };
    if (btn) btn.title = titles[mode] || 'Toggle theme';
  }

  applyTheme(savedTheme);
  updateThemeIcon(savedTheme);

  // Listen for OS preference changes when in system mode
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', function () {
    if ((localStorage.getItem('theme') || 'system') === 'system') applyTheme('system');
  });

  var themeToggle = document.getElementById('theme-toggle');
  if (themeToggle) {
    themeToggle.addEventListener('click', function () {
      var current = localStorage.getItem('theme') || 'system';
      var idx = themeCycle.indexOf(current);
      var next = themeCycle[(idx + 1) % themeCycle.length];
      localStorage.setItem('theme', next);
      applyTheme(next);
      updateThemeIcon(next);
    });
  }

  // --- Intersection Observer for scroll animations ---
  const observer = new IntersectionObserver(
    (entries) => {
      entries.forEach((entry) => {
        if (entry.isIntersecting) {
          entry.target.classList.add('animate-in');
          observer.unobserve(entry.target);
        }
      });
    },
    { threshold: 0.1, rootMargin: '0px 0px -40px 0px' }
  );

  document.querySelectorAll('[data-animate]').forEach((el) => {
    observer.observe(el);
  });

  // --- Sticky nav background on scroll ---
  const nav = document.getElementById('main-nav');
  if (nav) {
    window.addEventListener(
      'scroll',
      () => {
        if (window.scrollY > 40) {
          nav.classList.add('nav-scrolled');
        } else {
          nav.classList.remove('nav-scrolled');
        }
      },
      { passive: true }
    );
  }

  // --- Terminal typing effect ---
  const typingEl = document.getElementById('typing-output');
  if (typingEl) {
    const lines = [
      { text: '$ polyphony', delay: 80 },
      { text: '', delay: 400, isNewline: true },
      {
        text: '  +- Inbox -----------------------------------------+',
        delay: 20,
      },
      { text: '  | * #42  Fix auth token refresh         claude    |', delay: 20 },
      { text: '  | ~ #43  Add rate limiting              codex     |', delay: 20 },
      { text: '  | + #41  Update dependencies             pi       |', delay: 20 },
      {
        text: '  +----------------------------------- 3 of 12 -----+',
        delay: 20,
      },
    ];

    let lineIndex = 0;
    let charIndex = 0;
    let output = '';

    function typeNext() {
      if (lineIndex >= lines.length) return;

      const line = lines[lineIndex];
      if (line.isNewline) {
        output += '\n';
        typingEl.textContent = output;
        lineIndex++;
        charIndex = 0;
        setTimeout(typeNext, line.delay);
        return;
      }

      if (charIndex < line.text.length) {
        output += line.text[charIndex];
        typingEl.textContent = output;
        charIndex++;
        setTimeout(typeNext, line.delay);
      } else {
        output += '\n';
        typingEl.textContent = output;
        lineIndex++;
        charIndex = 0;
        setTimeout(typeNext, 100);
      }
    }

    // Start typing when hero is visible
    const heroObserver = new IntersectionObserver(
      (entries) => {
        if (entries[0].isIntersecting) {
          setTimeout(typeNext, 600);
          heroObserver.disconnect();
        }
      },
      { threshold: 0.3 }
    );
    heroObserver.observe(typingEl.closest('.terminal-chrome'));
  }

  // --- Copy to clipboard ---
  document.querySelectorAll('.copy-btn').forEach((btn) => {
    btn.addEventListener('click', () => {
      const text = btn.getAttribute('data-copy');
      navigator.clipboard.writeText(text).then(() => {
        btn.classList.add('copied');
        btn.textContent = 'Copied!';
        setTimeout(() => {
          btn.classList.remove('copied');
          btn.textContent = 'Copy';
        }, 2000);
      });
    });
  });

  // --- Install section: platform tab switching ---
  var installTabs = document.querySelectorAll('.install-tab');
  var installPanels = document.querySelectorAll('.install-panel');
  installTabs.forEach(function (tab) {
    tab.addEventListener('click', function () {
      installTabs.forEach(function (t) {
        t.classList.remove('bg-accent/20', 'text-accent-light');
        t.classList.add('text-gray-500');
      });
      tab.classList.add('bg-accent/20', 'text-accent-light');
      tab.classList.remove('text-gray-500');
      var target = tab.getAttribute('data-install');
      installPanels.forEach(function (p) {
        if (p.getAttribute('data-install-panel') === target) {
          p.classList.remove('hidden');
        } else {
          p.classList.add('hidden');
        }
      });
    });
  });

  // --- Smooth scroll for nav links ---
  document.querySelectorAll('a[href^="#"]').forEach((link) => {
    link.addEventListener('click', (e) => {
      const target = document.querySelector(link.getAttribute('href'));
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }
    });
  });

  // --- TUI Dashboard: tab switching ---
  const tuiDemo = document.getElementById('tui-demo');
  if (tuiDemo) {
    const tabs = tuiDemo.querySelectorAll('.tui-tab');
    const panels = tuiDemo.querySelectorAll('.tui-panel');
    const modal = document.getElementById('tui-detail-modal');
    const modalTitle = document.getElementById('tui-detail-title');
    const modalBody = document.getElementById('tui-detail-body');
    const modalClose = document.getElementById('tui-detail-close');

    tabs.forEach((tab) => {
      tab.addEventListener('click', () => {
        // Update tab styles
        tabs.forEach((t) => {
          t.classList.remove('bg-accent/20', 'text-accent-light', 'font-bold');
          t.classList.add('text-gray-500');
        });
        tab.classList.add('bg-accent/20', 'text-accent-light', 'font-bold');
        tab.classList.remove('text-gray-500');

        // Show matching panel
        const target = tab.getAttribute('data-tab');
        panels.forEach((p) => {
          if (p.getAttribute('data-panel') === target) {
            p.classList.remove('hidden');
          } else {
            p.classList.add('hidden');
          }
        });

        // Hide detail modal on tab switch
        modal.classList.add('hidden');
      });
    });

    // --- TUI Dashboard: detail modals on row click ---
    var details = {
      'inbox-42': {
        title: 'Inbox Item #42 — Fix auth token refresh',
        body: [
          ['Status',    '<span class="text-amber">running</span>'],
          ['Agent',     '<span class="text-accent-light">claude</span>'],
          ['Labels',    '<span class="text-amber-light">bug</span> <span class="text-muted">auth</span>'],
          ['Branch',    '<span class="text-muted">fix/auth-token-refresh</span>'],
          ['Workspace', '<span class="text-green-400">●</span> worktree active'],
          ['Steps',     '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-amber">⠋</span> agent running → <span class="text-gray-500">◷</span> commit → <span class="text-gray-500">◷</span> push → <span class="text-gray-500">◷</span> PR'],
          ['Body',      'Auth tokens expire after 1h but the refresh logic silently fails, leaving users logged out.'],
        ],
      },
      'inbox-43': {
        title: 'Inbox Item #43 — Add rate limiting middleware',
        body: [
          ['Status',    '<span class="text-green-400">done</span>'],
          ['Agent',     '<span class="text-accent-light">codex</span>'],
          ['Labels',    '<span class="text-accent-light">feature</span> <span class="text-muted">api</span>'],
          ['Branch',    '<span class="text-muted">feat/rate-limiting</span>'],
          ['Workspace', '<span class="text-muted">cleaned up</span>'],
          ['PR',        '<span class="text-green-400">✓</span> #87 merged'],
          ['Steps',     '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-green-400">✓</span> agent → <span class="text-green-400">✓</span> commit → <span class="text-green-400">✓</span> push → <span class="text-green-400">✓</span> PR'],
        ],
      },
      'inbox-44': {
        title: 'Inbox Item #44 — Update API documentation',
        body: [
          ['Status',    '<span class="text-gray-500">queued</span>'],
          ['Agent',     '<span class="text-accent-light">pi</span> (pending)'],
          ['Labels',    '<span class="text-muted">docs</span>'],
          ['Body',      'The API docs are out of date after the v2 endpoint changes. Regenerate from OpenAPI spec.'],
        ],
      },
      'inbox-45': {
        title: 'Inbox Item #45 — Refactor database connection pool',
        body: [
          ['Status',    '<span class="text-red-400">failed</span> (3 retries exhausted)'],
          ['Agent',     '<span class="text-accent-light">claude</span>'],
          ['Labels',    '<span class="text-muted">refactor</span> <span class="text-muted">database</span>'],
          ['Branch',    '<span class="text-muted">refactor/db-pool</span>'],
          ['Error',     '<span class="text-red-400">Agent exceeded token budget (150k) on all 3 attempts</span>'],
          ['Steps',     '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-red-400">✕</span> agent failed (3x)'],
        ],
      },
      'inbox-46': {
        title: 'Inbox Item #46 — Add WebSocket support',
        body: [
          ['Status',    '<span class="text-green-400">done</span>'],
          ['Agent',     '<span class="text-accent-light">codex</span> (retry 2 succeeded)'],
          ['Labels',    '<span class="text-accent-light">feature</span> <span class="text-muted">realtime</span>'],
          ['Branch',    '<span class="text-muted">feat/websocket</span>'],
          ['PR',        '<span class="text-green-400">✓</span> #89 open — awaiting review'],
          ['Steps',     '<span class="text-red-400">✕</span> attempt 1 (claude) → <span class="text-green-400">✓</span> attempt 2 (codex) → <span class="text-green-400">✓</span> commit → <span class="text-green-400">✓</span> push → <span class="text-green-400">✓</span> PR'],
        ],
      },
      'run-1': {
        title: 'Run ID run-0017 — #42 Fix auth token refresh',
        body: [
          ['Agent',     '<span class="text-accent-light">claude</span> via cli'],
          ['Attempt',   '1 of 3'],
          ['Duration',  '4m 12s (running)'],
          ['Tokens',    '48.2k / 150k budget'],
          ['Workspace', '<span class="text-green-400">●</span> /worktrees/fix-auth-token-refresh'],
          ['Pipeline',  '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-amber">⠋</span> agent running'],
          ['Stdout',    '<span class="text-muted">Analyzing auth module... Found refresh_token() in src/auth/token.rs. Implementing fix...</span>'],
        ],
      },
      'run-2': {
        title: 'Run ID run-0016 — #43 Add rate limiting',
        body: [
          ['Agent',     '<span class="text-accent-light">codex</span> via cli'],
          ['Attempt',   '1 of 3'],
          ['Duration',  '12m 47s'],
          ['Tokens',    '31.5k'],
          ['Result',    '<span class="text-green-400">✓</span> PR #87 merged'],
          ['Pipeline',  '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-green-400">✓</span> agent → <span class="text-green-400">✓</span> commit (3 files) → <span class="text-green-400">✓</span> push → <span class="text-green-400">✓</span> PR'],
          ['Files',     '<span class="text-muted">src/middleware/rate_limit.rs (+142), src/main.rs (+3), tests/rate_limit_test.rs (+67)</span>'],
        ],
      },
      'run-3': {
        title: 'Run ID run-0015 — #45 Refactor db pool',
        body: [
          ['Agent',     '<span class="text-accent-light">claude</span> via cli'],
          ['Attempt',   '3 of 3 — <span class="text-red-400">exhausted</span>'],
          ['Duration',  '8m 03s'],
          ['Tokens',    '<span class="text-red-400">150k / 150k (budget exceeded)</span>'],
          ['Pipeline',  '<span class="text-green-400">✓</span> clone → <span class="text-green-400">✓</span> checkout → <span class="text-red-400">✕</span> agent budget exceeded'],
          ['History',   'Attempt 1: budget (150k). Attempt 2: budget (150k). Attempt 3: budget (150k).'],
        ],
      },
      'run-4': {
        title: 'Run ID run-0014 — #46 Add WebSocket',
        body: [
          ['Agent',     '<span class="text-accent-light">codex</span> via cli (fallback from claude)'],
          ['Attempt',   '2 of 3'],
          ['Duration',  '21m 35s'],
          ['Tokens',    '57.7k'],
          ['Result',    '<span class="text-green-400">✓</span> PR #89 open'],
          ['Pipeline',  '<span class="text-red-400">✕</span> attempt 1 (claude, 6m) → <span class="text-green-400">✓</span> attempt 2 (codex) → <span class="text-green-400">✓</span> commit → <span class="text-green-400">✓</span> push → <span class="text-green-400">✓</span> PR'],
          ['Handoff',   'Claude failed with compile error. Codex picked up from clean worktree.'],
        ],
      },
      'run-5': {
        title: 'Run ID run-0013 — #44 Update API docs',
        body: [
          ['Agent',     '<span class="text-accent-light">pi</span> via acp'],
          ['Status',    '<span class="text-gray-500">◷ queued</span> — waiting for agent slot'],
          ['Priority',  '3 (low)'],
          ['Reason',    'Concurrency limit reached (2/2 agents active). Will start when a slot opens.'],
        ],
      },
      'agent-claude': {
        title: 'Agent: claude',
        body: [
          ['Transport', 'cli (claude-code)'],
          ['Status',    '<span class="text-amber">running</span> — working on #42'],
          ['Session',   '<span class="text-green-400">9</span> completed, <span class="text-red-400">1</span> failed'],
          ['Tokens',    '142.8k total this session'],
          ['Budget',    '150k per issue'],
          ['Config',    '<span class="text-muted">max_retries: 3, fallback: codex</span>'],
          ['Workspace', '<span class="text-green-400">●</span> /worktrees/fix-auth-token-refresh'],
        ],
      },
      'agent-codex': {
        title: 'Agent: codex',
        body: [
          ['Transport', 'cli (codex-cli)'],
          ['Status',    '<span class="text-green-400">● idle</span> — ready for work'],
          ['Session',   '<span class="text-green-400">5</span> completed, <span class="text-red-400">0</span> failed'],
          ['Tokens',    '89.2k total this session'],
          ['Budget',    '150k per issue'],
          ['Config',    '<span class="text-muted">max_retries: 3, fallback: none</span>'],
          ['Last job',  '#43 rate limiting — completed 15m ago'],
        ],
      },
      'agent-pi': {
        title: 'Agent: pi',
        body: [
          ['Transport', 'acp (agent communication protocol)'],
          ['Status',    '<span class="text-gray-500">◷ queued</span> — waiting for slot'],
          ['Session',   '<span class="text-green-400">1</span> completed, <span class="text-red-400">0</span> failed'],
          ['Tokens',    '12.4k total this session'],
          ['Budget',    '80k per issue'],
          ['Config',    '<span class="text-muted">max_retries: 2, fallback: claude</span>'],
          ['Next',      '#44 Update API documentation'],
        ],
      },
    };

    function showDetailRich(key) {
      var d = details[key];
      if (!d) return;
      modalTitle.textContent = d.title;
      modalBody.replaceChildren();
      var container = document.createElement('div');
      container.className = 'space-y-2';
      d.body.forEach(function (row) {
        var line = document.createElement('div');
        line.className = 'grid grid-cols-12 gap-2';

        var label = document.createElement('span');
        label.className = 'col-span-3 text-gray-500';
        label.textContent = row[0];

        var value = document.createElement('span');
        value.className = 'col-span-9';
        // Parse the known-safe hardcoded HTML for colored spans
        var parser = new DOMParser();
        var parsed = parser.parseFromString('<body>' + row[1] + '</body>', 'text/html');
        Array.from(parsed.body.childNodes).forEach(function (node) {
          value.appendChild(document.importNode(node, true));
        });

        line.appendChild(label);
        line.appendChild(value);
        container.appendChild(line);
      });
      modalBody.replaceChildren(container);
      modal.classList.remove('hidden');
    }

    tuiDemo.querySelectorAll('.tui-row[data-detail]').forEach(function (row) {
      row.addEventListener('click', function () {
        showDetailRich(row.getAttribute('data-detail'));
      });
    });

    modalClose.addEventListener('click', function () {
      modal.classList.add('hidden');
    });
  }
})();
