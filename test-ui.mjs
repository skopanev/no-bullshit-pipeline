import { webkit } from 'playwright';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { readFileSync } from 'fs';

const __dirname = dirname(fileURLToPath(import.meta.url));

async function testUI() {
  console.log('Starting UI tests...\n');

  const browser = await webkit.launch({ headless: true });
  const page = await browser.newPage();

  // Mock Tauri APIs
  await page.addInitScript(() => {
    window.__TAURI__ = {
      core: {
        invoke: async (cmd, args) => {
          console.log('Mock invoke:', cmd, args);

          // Mock responses
          if (cmd === 'load_settings') {
            return {
              storage_path: '/tmp/nbp-data',
              auto_discard_seconds: 3,
              theme: 'neon-purple',
              onboarding_completed: true,
              show_recording_notification: true,
              transcription: {
                enabled: true,
                provider: 'LocalWhisper',
                whisper_model: 'Base',
                api_keys: { openai: null, google: null, anthropic: null }
              }
            };
          }
          if (cmd === 'list_recordings') return [];
          if (cmd === 'get_app_version') return '0.4.0';
          if (cmd === 'check_permissions') return { mic: true, system_audio: true };
          if (cmd === 'list_templates') return [
            { name: 'meeting-notes', description: 'Meeting Notes' },
            { name: 'brainstorm', description: 'Brainstorm' },
            { name: 'journal', description: 'Journal' }
          ];
          return null;
        }
      },
      event: {
        listen: () => () => {}
      },
      dialog: {
        open: async () => null
      }
    };
    window.__TAURI_PLUGIN_OPENER__ = {
      openPath: async () => {}
    };
  });

  // Load the HTML file
  const htmlPath = join(__dirname, 'src', 'index.html');
  await page.goto(`file://${htmlPath}`);

  // Wait for JS to initialize
  await page.waitForTimeout(1000);

  let passed = 0;
  let failed = 0;

  async function test(name, fn) {
    try {
      await fn();
      console.log(`✅ ${name}`);
      passed++;
    } catch (e) {
      console.log(`❌ ${name}: ${e.message}`);
      failed++;
    }
  }

  // Test: Settings UI elements exist
  await test('Settings button exists', async () => {
    const btn = await page.$('#settings-btn');
    if (!btn) throw new Error('Settings button not found');
  });

  // Click settings to open
  await page.click('#settings-btn');
  await page.waitForTimeout(500);

  await test('OpenAI API key input exists', async () => {
    const input = await page.$('#settings-api-key-openai');
    if (!input) throw new Error('OpenAI API key input not found');
  });

  await test('Google API key input exists', async () => {
    const input = await page.$('#settings-api-key-google');
    if (!input) throw new Error('Google API key input not found');
  });

  await test('Anthropic API key input exists', async () => {
    const input = await page.$('#settings-api-key-anthropic');
    if (!input) throw new Error('Anthropic API key input not found');
  });

  await test('Recording notification toggle exists', async () => {
    const toggle = await page.$('#settings-recording-notification');
    if (!toggle) throw new Error('Recording notification toggle not found');
  });

  await test('Transcription provider has Anthropic option', async () => {
    const select = await page.$('#settings-transcription-provider');
    const options = await select?.$$eval('option', opts => opts.map(o => o.value));
    if (!options?.includes('Anthropic')) throw new Error('Anthropic option missing');
  });

  // Go back to main view
  await page.click('#settings-back-btn');
  await page.waitForTimeout(500);

  await test('Template selector exists', async () => {
    const select = await page.$('#template-select');
    if (!select) throw new Error('Template selector not found');
  });

  await test('Template options include Meeting Notes', async () => {
    const select = await page.$('#template-select');
    const options = await select?.$$eval('option', opts => opts.map(o => o.value));
    if (!options?.includes('meeting-notes')) throw new Error('meeting-notes option missing');
  });

  await test('Summarize button exists', async () => {
    const btn = await page.$('#summarize-btn');
    if (!btn) throw new Error('Summarize button not found');
  });

  await test('Extract button exists', async () => {
    const btn = await page.$('#extract-btn');
    if (!btn) throw new Error('Extract button not found');
  });

  await test('Waveform canvas exists', async () => {
    const canvas = await page.$('#recording-waveform-canvas');
    if (!canvas) throw new Error('Waveform canvas not found');
  });

  await test('Play/Pause button exists', async () => {
    const btn = await page.$('#play-pause-btn');
    if (!btn) throw new Error('Play/Pause button not found');
  });

  await browser.close();

  console.log(`\n========================================`);
  console.log(`Results: ${passed} passed, ${failed} failed`);
  console.log(`========================================`);

  process.exit(failed > 0 ? 1 : 0);
}

testUI().catch(e => {
  console.error('Test error:', e);
  process.exit(1);
});
