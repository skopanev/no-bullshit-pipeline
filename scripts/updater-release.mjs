// Release metadata is published only after all signed artifacts are uploaded.
import { readFileSync, writeFileSync, statSync } from 'node:fs';
import { resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';

const REPO = 'skopanev/no-bullshit-pipeline';

export function createManifest({ version, signature, archiveName = 'nbp.app.tar.gz', date = new Date().toISOString() }) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Invalid release version');
  if (archiveName !== 'nbp.app.tar.gz') throw new Error('Unexpected updater artifact name');
  const normalized = signature.trim();
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(normalized) || normalized.length < 100) throw new Error('Missing or invalid updater signature');
  if (!Number.isFinite(Date.parse(date))) throw new Error('Invalid release date');
  return {
    version,
    notes: `nbp ${version}`,
    pub_date: date,
    platforms: {
      'darwin-aarch64': {
        signature: normalized,
        url: `https://github.com/${REPO}/releases/download/v${version}/${archiveName}`,
      },
    },
  };
}

export function publishRelease({ version, dmg, archive, signature, manifest, gh }) {
  const tag = `v${version}`;
  // Listing distinguishes a missing tag from auth/network errors. Never
  // overwrite an already public release or make it latest prematurely.
  const releases = JSON.parse(gh(['api', `repos/${REPO}/releases`, '--paginate', '--slurp'])).flat();
  const existing = releases.find(release => release.tag_name === tag);
  if (existing && !existing.draft) throw new Error(`${tag} is already published; bump the version`);
  if (!existing) gh(['release', 'create', tag, '--repo', REPO, '--draft', '--title', `nbp ${tag}`, '--notes', `Release ${tag}`]);
  gh(['release', 'upload', tag, '--repo', REPO, dmg, archive, signature, '--clobber']);
  gh(['release', 'upload', tag, '--repo', REPO, manifest, '--clobber']);
  const uploaded = JSON.parse(gh(['release', 'view', tag, '--repo', REPO, '--json', 'assets'])).assets;
  for (const path of [dmg, archive, signature, manifest]) {
    if (!uploaded.some(asset => asset.name === basename(path) && asset.size === statSync(path).size)) {
      throw new Error(`Upload verification failed: ${basename(path)}; release remains draft`);
    }
  }
  gh(['release', 'edit', tag, '--repo', REPO, '--draft=false', '--latest']);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.platform !== 'darwin' || process.arch !== 'arm64') throw new Error('This release pipeline builds macOS Apple Silicon artifacts only');
  const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  const archive = 'src-tauri/target/release/bundle/macos/nbp.app.tar.gz';
  const signature = `${archive}.sig`;
  const dmg = `builds/nbp_v${config.version}.dmg`;
  const manifest = 'builds/latest.json';
  for (const path of [archive, signature, dmg]) if (!statSync(path).isFile() || statSync(path).size === 0) throw new Error(`Missing artifact: ${path}`);
  writeFileSync(manifest, JSON.stringify(createManifest({ version: config.version, signature: readFileSync(signature, 'utf8') }), null, 2) + '\n');
  publishRelease({ version: config.version, dmg, archive, signature, manifest,
    gh: args => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] }),
  });
}
