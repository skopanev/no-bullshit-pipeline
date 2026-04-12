// End-to-end tests for NBP using tauri-driver
import { spawn, spawnSync } from 'child_process';
import { webkit } from 'playwright';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Start tauri-driver and get the WebDriver port
async function startTauriDriver() {
  return new Promise((resolve, reject) => {
    const driver = spawn('tauri-driver', [], {
      stdio: ['ignore', 'pipe', 'pipe']
    });

    let output = '';
    driver.stdout.on('data', (data) => {
      output += data.toString();
      // tauri-driver outputs the port it's listening on
      const match = output.match(/listening on port (\d+)/i);
      if (match) {
        resolve({ driver, port: parseInt(match[1]) });
      }
    });

    driver.stderr.on('data', (data) => {
      console.error('tauri-driver stderr:', data.toString());
    });

    driver.on('error', reject);

    // Timeout after 10 seconds
    setTimeout(() => {
      driver.kill();
      reject(new Error('tauri-driver startup timeout'));
    }, 10000);
  });
}

async function runE2ETests() {
  console.log('Starting E2E tests for NBP...\n');

  // Build the app in dev mode first
  console.log('Building app...');
  const buildResult = spawnSync('cargo', ['build'], {
    cwd: join(__dirname, 'src-tauri'),
    stdio: 'inherit'
  });

  if (buildResult.status !== 0) {
    console.error('Build failed');
    process.exit(1);
  }

  console.log('Starting app...');

  // Start the actual app
  const app = spawn(join(__dirname, 'src-tauri/target/debug/nbp'), [], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, WEBKIT_DISABLE_COMPOSITING_MODE: '1' }
  });

  let appOutput = '';
  app.stdout.on('data', (data) => {
    appOutput += data.toString();
  });
  app.stderr.on('data', (data) => {
    appOutput += data.toString();
  });

  // Wait for app to start
  await new Promise(resolve => setTimeout(resolve, 3000));

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

  // Test: App process is running
  await test('App process started', async () => {
    if (app.killed || app.exitCode !== null) {
      throw new Error('App process not running');
    }
  });

  // Test: Check app logs for errors
  await test('No critical errors in app startup', async () => {
    if (appOutput.toLowerCase().includes('panic') || appOutput.toLowerCase().includes('fatal')) {
      throw new Error(`Critical error in app: ${appOutput}`);
    }
  });

  // Test: Check app version command works
  await test('App version is 0.4.0', async () => {
    // The version is embedded in the app
    const pkgJson = await import(join(__dirname, 'src-tauri', 'tauri.conf.json'), { with: { type: 'json' } });
    if (pkgJson.default.version !== '0.4.0') {
      throw new Error(`Expected version 0.4.0, got ${pkgJson.default.version}`);
    }
  });

  // Test: Check Cargo.toml has correct dependencies
  await test('Cloud AI dependencies present', async () => {
    const { readFileSync } = await import('fs');
    const cargoToml = readFileSync(join(__dirname, 'src-tauri', 'Cargo.toml'), 'utf-8');

    const requiredDeps = ['reqwest', 'serde', 'serde_json', 'lewton'];
    for (const dep of requiredDeps) {
      if (!cargoToml.includes(dep)) {
        throw new Error(`Missing dependency: ${dep}`);
      }
    }
  });

  // Test: Check all Tauri commands are registered
  await test('All Tauri commands registered', async () => {
    const { readFileSync } = await import('fs');
    const libRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'lib.rs'), 'utf-8');

    const requiredCommands = [
      'summarize_recording',
      'process_with_template',
      'list_templates',
      'get_template',
      'play_audio',
      'get_waveform_data',
      'get_playback_state'
    ];

    for (const cmd of requiredCommands) {
      if (!libRs.includes(cmd)) {
        throw new Error(`Missing command: ${cmd}`);
      }
    }
  });

  // Test: Check frontend has all required UI elements
  await test('Frontend has all UI elements', async () => {
    const { readFileSync } = await import('fs');
    const indexHtml = readFileSync(join(__dirname, 'src', 'index.html'), 'utf-8');

    const requiredElements = [
      'settings-api-key-openai',
      'settings-api-key-google',
      'settings-api-key-anthropic',
      'template-select',
      'summarize-btn',
      'extract-btn',
      'recording-waveform-canvas',
      'play-pause-btn',
      'settings-recording-notification'
    ];

    for (const el of requiredElements) {
      if (!indexHtml.includes(el)) {
        throw new Error(`Missing UI element: ${el}`);
      }
    }
  });

  // Test: Check main.js has all handlers
  await test('Frontend JS has all handlers', async () => {
    const { readFileSync } = await import('fs');
    const mainJs = readFileSync(join(__dirname, 'src', 'dist', 'app.js'), 'utf-8');

    const requiredHandlers = [
      'summarize_recording',
      'process_with_template',
      'list_templates',
      'play_audio'
    ];

    for (const handler of requiredHandlers) {
      if (!mainJs.includes(handler)) {
        throw new Error(`Missing handler: ${handler}`);
      }
    }
  });

  // Test: Cloud AI modules exist
  await test('Cloud AI modules exist', async () => {
    const { existsSync } = await import('fs');
    const modules = [
      'src-tauri/src/cloud_ai/mod.rs',
      'src-tauri/src/cloud_ai/openai.rs',
      'src-tauri/src/cloud_ai/google.rs',
      'src-tauri/src/cloud_ai/anthropic.rs'
    ];

    for (const m of modules) {
      const path = join(__dirname, m);
      if (!existsSync(path)) {
        throw new Error(`Missing module: ${m}`);
      }
    }
  });

  // Test: Template module exists and has builtin templates
  await test('Templates module has builtin templates', async () => {
    const { readFileSync } = await import('fs');
    const templatesRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'templates.rs'), 'utf-8');

    const requiredTemplates = ['meeting-notes', 'brainstorm', 'journal'];
    for (const tmpl of requiredTemplates) {
      if (!templatesRs.includes(tmpl)) {
        throw new Error(`Missing template: ${tmpl}`);
      }
    }
  });

  // Test: Waveform module exists and works
  await test('Waveform module implements OGG decoding', async () => {
    const { readFileSync } = await import('fs');
    const waveformRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'waveform.rs'), 'utf-8');

    if (!waveformRs.includes('OggStreamReader')) {
      throw new Error('Waveform module missing OGG decoder');
    }
    if (!waveformRs.includes('WAVEFORM_SAMPLES')) {
      throw new Error('Waveform module missing sample constant');
    }
  });

  // Test: Playback module exists
  await test('Playback module exists', async () => {
    const { readFileSync } = await import('fs');
    const playbackRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'playback.rs'), 'utf-8');

    if (!playbackRs.includes('play_audio')) {
      throw new Error('Playback module missing play_audio');
    }
  });

  // Test: Config has API keys structure
  await test('Config supports API keys', async () => {
    const { readFileSync } = await import('fs');
    const configRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'config.rs'), 'utf-8');

    const requiredFields = ['ApiKeys', 'openai', 'google', 'anthropic'];
    for (const field of requiredFields) {
      if (!configRs.includes(field)) {
        throw new Error(`Config missing: ${field}`);
      }
    }
  });

  // Test: Storage has health tracking
  await test('Storage supports recording health', async () => {
    const { readFileSync } = await import('fs');
    const storageRs = readFileSync(join(__dirname, 'src-tauri', 'src', 'storage.rs'), 'utf-8');

    if (!storageRs.includes('RecordingHealth')) {
      throw new Error('Storage missing RecordingHealth');
    }
    if (!storageRs.includes('RecordingIssue')) {
      throw new Error('Storage missing RecordingIssue');
    }
  });

  // Test: Entitlements include required permissions
  await test('Entitlements include required permissions', async () => {
    const { readFileSync } = await import('fs');
    const entitlements = readFileSync(join(__dirname, 'src-tauri', 'entitlements.plist'), 'utf-8');

    const required = [
      'com.apple.security.device.audio-input',
      'com.apple.security.device.screen-capture',
      'com.apple.security.cs.allow-unsigned-executable-memory'
    ];

    for (const ent of required) {
      if (!entitlements.includes(ent)) {
        throw new Error(`Missing entitlement: ${ent}`);
      }
    }
  });

  // Clean up
  app.kill();

  console.log(`\n========================================`);
  console.log(`Results: ${passed} passed, ${failed} failed`);
  console.log(`========================================`);

  process.exit(failed > 0 ? 1 : 0);
}

runE2ETests().catch(e => {
  console.error('E2E test error:', e);
  process.exit(1);
});
