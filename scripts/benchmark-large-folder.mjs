import { spawnSync } from 'node:child_process';

const count = process.argv[2] ?? '100000';
for (const mode of ['legacy', 'shared']) {
  const result = spawnSync(
    'cargo',
    ['run', '-p', 'explorie-native-services', '--example', 'large_folder_profile', '--release', '--', count, mode],
    { cwd: process.cwd(), encoding: 'utf8', shell: process.platform === 'win32' },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  process.stdout.write(result.stdout);
}
