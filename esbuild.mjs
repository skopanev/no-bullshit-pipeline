import * as esbuild from 'esbuild';

const isWatch = process.argv.includes('--watch');
const isDev = process.argv.includes('--dev') || isWatch;

const ctx = await esbuild.context({
  entryPoints: ['src/js/main.js'],
  bundle: true,
  outfile: 'src/dist/app.js',
  format: 'iife',
  target: ['es2022'],
  sourcemap: isDev,
  minify: !isDev,
  logLevel: 'info',
});

if (isWatch) {
  await ctx.watch();
  console.log('esbuild watching...');
} else {
  await ctx.rebuild();
  await ctx.dispose();
}
