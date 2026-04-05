export function looksLikeMarkdown(text) {
  if (!text || text.length < 20) return false;
  return /^#{1,6}\s/m.test(text) || /\*\*.+?\*\*/s.test(text) ||
    /^[-*]\s/m.test(text) || /^\d+\.\s/m.test(text) ||
    /^```/m.test(text) || /^>\s/m.test(text);
}

export function applyMarkdownRendering(el, rawText) {
  if (!el || !rawText) return;
  el.dataset.rawText = rawText;
  if (looksLikeMarkdown(rawText)) {
    el.innerHTML = window.marked.parse(rawText);
    el.classList.add('md-rendered');
    el.classList.remove('md-raw');
  } else {
    el.textContent = rawText;
    el.classList.remove('md-rendered');
    el.classList.add('md-raw');
  }
}
