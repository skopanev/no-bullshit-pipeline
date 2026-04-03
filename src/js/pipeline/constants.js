// Pipeline constants: SVG icons, connector metadata, provider metadata

export const SLACK_SVG = `<svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor"><path d="M5.042 15.165a2.528 2.528 0 0 1-2.52 2.523A2.528 2.528 0 0 1 0 15.165a2.527 2.527 0 0 1 2.522-2.52h2.52v2.52zm1.271 0a2.527 2.527 0 0 1 2.521-2.52 2.527 2.527 0 0 1 2.521 2.52v6.313A2.528 2.528 0 0 1 8.834 24a2.528 2.528 0 0 1-2.521-2.522v-6.313zm2.521-10.123a2.528 2.528 0 0 1-2.521-2.52A2.528 2.528 0 0 1 8.834 0a2.528 2.528 0 0 1 2.521 2.522v2.52H8.834zm0 1.271a2.528 2.528 0 0 1 2.521 2.521 2.528 2.528 0 0 1-2.521 2.521H2.522A2.528 2.528 0 0 1 0 8.834a2.528 2.528 0 0 1 2.522-2.521h6.312zm10.123 2.521a2.528 2.528 0 0 1 2.522-2.521A2.528 2.528 0 0 1 24 8.834a2.528 2.528 0 0 1-2.522 2.521h-2.522V8.834zm-1.268 0a2.528 2.528 0 0 1-2.523 2.521 2.527 2.527 0 0 1-2.52-2.521V2.522A2.527 2.527 0 0 1 15.165 0a2.528 2.528 0 0 1 2.523 2.522v6.312zm-2.523 10.123a2.528 2.528 0 0 1 2.523 2.522A2.528 2.528 0 0 1 15.165 24a2.527 2.527 0 0 1-2.52-2.522v-2.522h2.52zm0-1.268a2.527 2.527 0 0 1-2.52-2.523 2.526 2.526 0 0 1 2.52-2.52h6.313A2.527 2.527 0 0 1 24 15.165a2.528 2.528 0 0 1-2.522 2.523h-6.313z"/></svg>`;

export const NOTION_SVG = `<svg viewBox="0 0 100 100" width="18" height="18" fill="currentColor"><path d="M6.017 4.313l55.333-4.087c6.797-.583 8.543-.19 12.817 2.917l17.663 12.443c2.913 2.14 3.883 2.723 3.883 5.053v68.243c0 4.277-1.553 6.807-6.99 7.193L24.467 99.967c-4.08.193-6.023-.39-8.16-3.113L3.3 79.94c-2.333-3.113-3.3-5.443-3.3-8.167V11.113c0-3.497 1.553-6.413 6.017-6.8z" fill="#fff"/><path d="M61.35.227l-55.333 4.087C1.553 4.7 0 7.617 0 11.113v60.66c0 2.723.967 5.053 3.3 8.167l13.007 16.913c2.137 2.723 4.08 3.307 8.16 3.113l64.257-3.89c5.433-.387 6.99-2.917 6.99-7.193V20.64c0-2.21-.81-2.76-3.088-4.587L75.983 3.523C71.71.607 69.96.22 63.163.803L61.35.227z" fill="#000"/><path d="M26.395 18.768c-5.433.39-6.675.477-9.768-1.753L7.997 10.527c-1.163-.913-1.55-1.94-1.55-3.113.39-2.53 1.94-4.47 7.377-4.86l53.39-3.89c4.47-.39 6.603 1.553 8.157 2.723l10.133 7.577c.39.193 1.553 1.553 0 1.553l-55.14 3.11v5.14z" fill="#fff"/><path d="M19.018 88.4V30.173c0-2.527.78-3.697 3.113-3.89l57.277-3.307c2.14-.193 3.113 1.167 3.113 3.693V85.09c0 2.527-.39 4.667-3.887 4.86l-54.943 3.113c-3.5.193-4.673-1.003-4.673-4.663zm54.167-55.13c.39 1.75 0 3.5-1.75 3.697l-2.527.39v40.257c-2.14 1.163-4.277 1.75-5.833 1.75-2.723 0-3.5-.583-5.443-3.113L38.468 45.948V74.7l5.247 1.163s0 3.5-4.86 3.5l-13.393.78c-.39-.78 0-2.723 1.36-3.113l3.497-.97V38.33l-4.86-.39c-.39-1.75.583-4.277 3.307-4.473l14.363-.97 20.603 31.46V35.077l-4.47-.39c-.39-2.14 1.163-3.697 3.113-3.89l14.003-.527z" fill="#fff"/></svg>`;

export const LINEAR_SVG = `<svg viewBox="0 0 100 100" width="18" height="18"><path d="M2.76 62.7a50.1 50.1 0 0 1-1.52-4.44L62.7 2.76a50.1 50.1 0 0 0-4.44-1.52L2.76 62.7zm7.66 12.48a50 50 0 0 1-3.54-4.3L75.18 4.58a50 50 0 0 0-4.3-3.54L10.42 75.18zm11.44 8.96a50 50 0 0 1-4.82-4.1L83.14 13.94a50 50 0 0 0-4.1-4.82L21.86 84.14zM0 50a49.9 49.9 0 0 0 .26 5L55 .26A50 50 0 1 0 0 50zm35.42 36.64a50 50 0 0 1-5.36-3.72L86.92 16.64a50 50 0 0 0-3.72-5.36L35.42 86.64z" fill="#5E6AD2"/></svg>`;

export const SAVE_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>`;

export const WEBHOOK_SVG = `<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 007.54.54l3-3a5 5 0 00-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 00-7.54-.54l-3 3a5 5 0 007.07 7.07l1.71-1.71"/></svg>`;

export const CLI_SVG = `<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>`;

export const PROVIDER_META = {
  openai:    { img: 'assets/openai.svg',    filter: 'none', bgColor: 'transparent' },
  google:    { img: 'assets/gemini.svg',     filter: 'none', bgColor: 'transparent' },
  anthropic: { img: 'assets/anthropic.svg',  filter: 'none', bgColor: 'transparent' },
  local:     { img: 'assets/local-llm.svg',  filter: 'none', bgColor: 'transparent' },
  ollama:    { img: 'assets/ollama.svg',     filter: 'none', bgColor: 'transparent' },
};

export const CONNECTOR_META = {
  llm:       { abbr: 'AI',  textColor: 'var(--accent)',  bgColor: 'var(--accent-soft)' },
  cli_agent: { svg: CLI_SVG, textColor: '#fff',          bgColor: 'rgba(99,102,241,0.9)' },
  save:      { svg: SAVE_SVG,    textColor: '#10b981',   bgColor: 'rgba(16,185,129,0.15)' },
  slack:     { svg: SLACK_SVG,   textColor: '#fff',      bgColor: '#4A154B' },
  notion:    { svg: NOTION_SVG,  textColor: '#fff',      bgColor: '#2f2f2f' },
  webhook:   { svg: WEBHOOK_SVG, textColor: '#60a5fa',   bgColor: 'rgba(59,130,246,0.2)' },
  linear:    { svg: LINEAR_SVG,  textColor: '#fff',      bgColor: '#5E6AD2' },
  mcp:       { abbr: 'MCP', textColor: '#f59e0b',        bgColor: 'rgba(245,158,11,0.15)' },
};

export const FALLBACK_CLI_INFO = [
  {
    id: 'claude',
    name: 'Claude Code',
    installed: false,
    install_hint: 'npm install -g @anthropic-ai/claude-code',
    models: [
      { id: 'sonnet', name: 'Claude Sonnet (latest)' },
      { id: 'opus', name: 'Claude Opus (latest)' },
      { id: 'haiku', name: 'Claude Haiku (latest)' },
      { id: 'claude-sonnet-4-20250514', name: 'Claude Sonnet 4' },
      { id: 'claude-opus-4-20250514', name: 'Claude Opus 4' },
    ],
    providers: [],
  },
  {
    id: 'codex',
    name: 'Codex CLI',
    installed: false,
    install_hint: 'npm install -g @openai/codex',
    models: [
      { id: 'o3', name: 'O3' },
      { id: 'o4-mini', name: 'O4 Mini' },
      { id: 'o3-mini', name: 'O3 Mini' },
    ],
    providers: [],
  },
  {
    id: 'opencode',
    name: 'OpenCode',
    installed: false,
    install_hint: 'npm install -g opencode-ai',
    models: [
      { id: 'openai/gpt-4o', name: 'GPT-4o' },
      { id: 'openai/gpt-4o-mini', name: 'GPT-4o Mini' },
      { id: 'openai/o3-mini', name: 'O3 Mini' },
      { id: 'anthropic/claude-sonnet-4-20250514', name: 'Claude Sonnet 4' },
      { id: 'anthropic/claude-opus-4-20250514', name: 'Claude Opus 4' },
      { id: 'anthropic/claude-3-5-sonnet-20241022', name: 'Claude 3.5 Sonnet' },
      { id: 'google/gemini-2.5-pro-preview-06-05', name: 'Gemini 2.5 Pro' },
      { id: 'google/gemini-2.0-flash', name: 'Gemini 2.0 Flash' },
    ],
    providers: [
      { id: 'openai', name: 'OpenAI' },
      { id: 'anthropic', name: 'Anthropic' },
      { id: 'google', name: 'Google' },
      { id: 'opencode', name: 'OpenCode (free)' },
      { id: 'zai-coding-plan', name: 'ZAI Coding Plan' },
    ],
  },
];
