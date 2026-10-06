import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, copyFile, mkdir, mkdtemp, rename, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const RCLONE_VERSION = 'v1.74.4';

export const RCLONE_TARGETS = {
  'x86_64-pc-windows-msvc': {
    archive: 'windows-amd64',
    sha256: 'ef097ef9de37a57feb7d9f9c7afb34148ad3c65be8025f1d8f7f521554a701ea',
    executable: 'rclone.exe',
  },
  'aarch64-pc-windows-msvc': {
    archive: 'windows-arm64',
    sha256: '72194ad0aaf210d7a55808801191fecc7e175444dab7be7491b7a63074521f3a',
    executable: 'rclone.exe',
  },
  'x86_64-apple-darwin': {
    archive: 'osx-amd64',
    sha256: '4188aa84043d7a6240912923f47639a9d2da21f3b40a521c065c8d92e66563f6',
    executable: 'rclone',
  },
  'aarch64-apple-darwin': {
    archive: 'osx-arm64',
    sha256: 'c2100e2d4a4b3be04c55cd45380cafe7647e1ad772bb055f52f00876ed701167',
    executable: 'rclone',
  },
};

export function nativeTarget(platform = process.platform, arch = process.arch) {
  const target = {
    'win32:x64': 'x86_64-pc-windows-msvc',
    'win32:arm64': 'aarch64-pc-windows-msvc',
    'darwin:x64': 'x86_64-apple-darwin',
    'darwin:arm64': 'aarch64-apple-darwin',
  }[`${platform}:${arch}`];
  if (!target) throw new Error(`Unsupported rclone sidecar platform: ${platform}/${arch}`);
  return target;
}

export async function prepareRclone(
  target = process.env.CARGO_BUILD_TARGET ?? nativeTarget()
) {
  const asset = RCLONE_TARGETS[target];
  if (!asset) throw new Error(`Unsupported rclone sidecar target: ${target}`);

  const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const binaries = path.join(repository, 'apps', 'desktop', 'native-assets', 'binaries');
  const destination = path.join(
    binaries,
    `rclone-${target}${asset.executable.endsWith('.exe') ? '.exe' : ''}`
  );
  await mkdir(binaries, { recursive: true });

  try {
    const version = execFileSync(destination, ['version'], { encoding: 'utf8' });
    if (version.startsWith(`rclone ${RCLONE_VERSION}`)) return destination;
  } catch {
    // Missing or stale sidecar: replace it from the pinned archive below.
  }

  const archiveName = `rclone-${RCLONE_VERSION}-${asset.archive}.zip`;
  const response = await fetch(`https://downloads.rclone.org/${RCLONE_VERSION}/${archiveName}`);
  if (!response.ok) throw new Error(`Failed to download ${archiveName}: HTTP ${response.status}`);
  const archive = Buffer.from(await response.arrayBuffer());
  const digest = createHash('sha256').update(archive).digest('hex');
  if (digest !== asset.sha256) throw new Error(`SHA-256 mismatch for ${archiveName}`);

  const temporary = await mkdtemp(path.join(os.tmpdir(), 'explorie-rclone-'));
  try {
    const zip = path.join(temporary, archiveName);
    const extracted = path.join(temporary, 'extracted');
    await mkdir(extracted);
    await writeFile(zip, archive);
    execFileSync('tar', ['-xf', zip, '-C', extracted], { stdio: 'inherit' });
    const source = path.join(
      extracted,
      `rclone-${RCLONE_VERSION}-${asset.archive}`,
      asset.executable
    );
    await stat(source);
    const staged = `${destination}.tmp`;
    await copyFile(source, staged);
    await chmod(staged, 0o755);
    await rm(destination, { force: true });
    await rename(staged, destination);
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }

  const version = execFileSync(destination, ['version'], { encoding: 'utf8' });
  if (!version.startsWith(`rclone ${RCLONE_VERSION}`)) {
    throw new Error(`Prepared sidecar does not report rclone ${RCLONE_VERSION}`);
  }
  return destination;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  prepareRclone(process.argv[2]).then((destination) => console.log(`Prepared ${destination}`));
}
