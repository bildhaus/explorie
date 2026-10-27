import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { nativeTarget, packagePlugins } from './package-plugins.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
function run(command, args, env = process.env) {
  const result = spawnSync(command, args, { cwd: root, env, stdio: 'inherit' });
  if (result.error || result.status !== 0) throw new Error(`${command} failed`, { cause: result.error });
}
try {
  run(process.execPath, ['scripts/prepare-7zip.mjs']);
  const catalog = await packagePlugins({ target: nativeTarget() });
  run('cargo', ['build', '-p', 'explorie-gpui', '--release', '--locked'], {
    ...process.env, EXPLORIE_PLUGIN_CATALOG: catalog,
  });
} catch (error) { console.error(error.message); process.exitCode = 1; }
