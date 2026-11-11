(() => {
  const key = 'repoconvoy.theme';
  const root = document.documentElement;
  const readCookie = () => document.cookie.match(/(?:^|;\s*)repoconvoy_theme=(dark|light)(?:;|$)/)?.[1];
  let mode = 'dark';
  try {
    const stored = readCookie() || localStorage.getItem(key);
    if (stored === 'light') mode = 'light';
  } catch { /* Browser storage may be disabled; dark remains usable. */ }
  const button = document.getElementById('theme-toggle');
  function render() {
    root.dataset.theme = mode;
    if (!button) return;
    const next = mode === 'dark' ? '화이트 모드' : '다크 모드';
    button.querySelector('.theme-label').textContent = next;
    button.querySelector('svg use').setAttribute('href', `/icons.svg#${mode === 'dark' ? 'sun' : 'moon'}`);
    button.setAttribute('aria-label', `${next}로 전환`);
  }
  button?.addEventListener('click', () => {
    mode = mode === 'dark' ? 'light' : 'dark';
    try {
      localStorage.setItem(key, mode);
      document.cookie = `repoconvoy_theme=${mode}; Path=/; SameSite=Strict; Max-Age=31536000`;
    } catch { /* Non-persistent mode still works. */ }
    render();
  });
  render();
})();
