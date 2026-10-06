import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { nativeTarget } from './prepare-rclone.mjs';

export const SEVENZIP_VERSION = '26.03';
export const SEVENZIP_SOURCE = {
  archive: '7z2603-src.tar.xz',
  sha256: '9cbde5099c6deb73691b0579063da5827522ccbbcba3f0020fd04e8c8c16c0d4',
};
const windowsX64 = {
  archive: '7z2603-x64.exe',
  sha256: '0859c524b8a63551848f0c246abddcb1d0b7b656b0fbfe879f8d85e61a9e6edd',
  files: {
    '7z.exe': '6ee3c0ed0b27663c1b948ae85a7c0bb073aed1498983182f3f0df1f6a8c30b2f',
    '7z.dll': '65e4c1f855f9ef6e8f0f5df8e3f27d9eb5f07311408639da0a1ca0b8f4871b0d',
  },
};
const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const cache = path.join(repository, '.cache', '7zip');
const macos = {
  archive: '7z2603-mac.tar.xz',
  sha256: '5ca87677072c59f5602e5c49baa27d4694bacd2259b4e507f0094249d4281480',
  files: { '7zz': '74b0910e50ea44d9760a57fada2192cfd530ba8bffbe7b47c412a464b796cabf' },
};
export const SEVENZIP_TARGETS = {
  'x86_64-pc-windows-msvc': windowsX64,
  'aarch64-pc-windows-msvc': {
    archive: '7z2603-arm64.exe',
    sha256: 'e22ce71c11dcf503c448fe51e56f41830eb4e1344fa5c7731ae63bce533a8e8e',
    files: {
      '7z.exe': '12cd5e3050ae377b2e8ce900d576671e4fcaac13b055677599cdbf2173e5ea04',
      '7z.dll': 'a1f2e41eaf7ad40f5b1aa66a73e7fe327e64e0b7d65638a863c1e3a6fe64e667',
    },
  },
  'x86_64-apple-darwin': macos,
  'aarch64-apple-darwin': macos,
};

export function verifySha256(bytes, expected, name) {
  const actual = createHash('sha256').update(bytes).digest('hex');
  if (actual !== expected) throw new Error(`SHA-256 mismatch for ${name}`);
}

async function download(asset, directory) {
  const destination = path.join(directory, asset.archive);
  try {
    verifySha256(await readFile(destination), asset.sha256, asset.archive);
    return destination;
  } catch {
    // Missing or changed cache: obtain the pinned upstream bytes again.
  }
  const url = `https://github.com/ip7z/7zip/releases/download/${SEVENZIP_VERSION}/${asset.archive}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`Failed to download ${asset.archive}: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  verifySha256(bytes, asset.sha256, asset.archive);
  await mkdir(directory, { recursive: true });
  await writeFile(destination, bytes);
  return destination;
}

async function verifyFiles(directory, files) {
  for (const [name, digest] of Object.entries(files)) {
    verifySha256(await readFile(path.join(directory, name)), digest, name);
  }
}

export async function stageSevenZipSource(directory) {
  const source = await download(SEVENZIP_SOURCE, cache);
  await mkdir(directory, { recursive: true });
  const destination = path.join(directory, SEVENZIP_SOURCE.archive);
  await copyFile(source, destination);
  await chmod(destination, 0o644);
  return destination;
}

export async function prepareSevenZip(target = process.env.CARGO_BUILD_TARGET ?? nativeTarget()) {
  const asset = SEVENZIP_TARGETS[target];
  if (!asset) throw new Error(`Unsupported 7-Zip sidecar target: ${target}`);
  const assets = path.join(repository, 'apps', 'desktop', 'native-assets');
  const destination = path.join(assets, 'binaries', `7zip-${target}`);
  // Keep the corresponding source ready to embed beside the installer licenses.
  await download(SEVENZIP_SOURCE, cache);

  try {
    await verifyFiles(destination, asset.files);
    return destination;
  } catch {
    // Never execute an unverified cached executable to detect its version.
  }
  const archive = await download(asset, cache);
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'explorie-7zip-'));
  try {
    const extracted = path.join(temporary, 'extracted');
    await mkdir(extracted);
    if (target === 'aarch64-pc-windows-msvc') {
      // Windows tar cannot decode the ARM64 BCJ filter. Bootstrap with the
      // verified x64 engine (also executable under Windows 11 ARM64 emulation).
      const bootstrap = path.join(temporary, 'bootstrap');
      await mkdir(bootstrap);
      const x64Archive = await download(windowsX64, cache);
      execFileSync('tar', ['-xf', x64Archive, '-C', bootstrap, '7z.exe', '7z.dll']);
      await verifyFiles(bootstrap, windowsX64.files);
      execFileSync(path.join(bootstrap, '7z.exe'), ['x', archive, `-o${extracted}`, '-y', '7z.exe', '7z.dll'], { windowsHide: true });
    } else {
      execFileSync('tar', ['-xf', archive, '-C', extracted, ...Object.keys(asset.files)]);
    }
    await verifyFiles(extracted, asset.files);
    await mkdir(destination, { recursive: true });
    for (const name of Object.keys(asset.files)) {
      await copyFile(path.join(extracted, name), path.join(destination, name));
      await chmod(path.join(destination, name), 0o755);
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
  return destination;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [argument, destination] = process.argv.slice(2);
  if (argument === '--stage-source' || argument === '--verify-source') {
    if (!destination) throw new Error(`${argument} requires a path`);
    if (argument === '--stage-source') {
      console.log(`Prepared ${await stageSevenZipSource(destination)}`);
    } else {
      verifySha256(await readFile(destination), SEVENZIP_SOURCE.sha256, SEVENZIP_SOURCE.archive);
      console.log(`Verified ${destination}`);
    }
  } else {
    console.log(`Prepared ${await prepareSevenZip(argument)}`);
  }
}
