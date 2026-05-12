import * as esbuild from 'esbuild';

const isWatch = process.argv.includes('--watch');
const isDev = process.argv.includes('--dev') || isWatch;

const sharedOptions = {
  bundle: true,
  target: ['es2022'],
  sourcemap: isDev,
  minify: !isDev,
  logLevel: 'info',
};

const mainCtx = await esbuild.context({
  ...sharedOptions,
  entryPoints: ['src/js/main.js'],
  outfile: 'src/dist/app.js',
  format: 'iife',
});

const hudCtx = await esbuild.context({
  ...sharedOptions,
  entryPoints: ['src/js/dictation-hud.js'],
  outfile: 'src/dist/dictation-hud.js',
  format: 'esm',
});

if (isWatch) {
  await Promise.all([mainCtx.watch(), hudCtx.watch()]);
  console.log('esbuild watching...');
} else {
  await Promise.all([mainCtx.rebuild(), hudCtx.rebuild()]);
  await Promise.all([mainCtx.dispose(), hudCtx.dispose()]);
}
