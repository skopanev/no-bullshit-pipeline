export function applyTheme(theme) {
  if (theme === 'light-pastel') theme = 'light';
  if (theme === 'deep-obsidian') theme = 'neon-purple';
  document.body.classList.remove('neon-purple', 'deep-blue', 'light');
  if (theme !== 'neon-purple') document.body.classList.add(theme);
  document.querySelectorAll('.theme-btn').forEach(b => b.classList.toggle('active', b.dataset.theme === theme));
}
