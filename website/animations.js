// Polyphony Website — Scroll Animations & Interactions

(function () {
  'use strict';

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
        text: '  ╭─ Issues ─────────────────────────────────╮',
        delay: 20,
      },
      { text: '  │ ● #42  Fix auth token refresh    claude  │', delay: 20 },
      { text: '  │ ⠋ #43  Add rate limiting         codex   │', delay: 20 },
      { text: '  │ ✓ #41  Update dependencies        pi     │', delay: 20 },
      {
        text: '  ╰──────────────────────────── 3 of 12 ─────╯',
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
})();
