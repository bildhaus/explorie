import { createHash } from 'node:crypto';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const WINFSP_VERSION = '2.1.25156';
export const WINFSP_SHA256 = '073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a';
export const WINFSP_INSTALLER = `winfsp-${WINFSP_VERSION}.msi`;

const sha256 = (value) => createHash('sha256').update(value).digest('hex');

export async function prepareWinFsp(platform = process.platform) {
  if (platform !== 'win32') return null;

  const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const resources = path.join(repository, 'apps', 'desktop', 'native-assets', 'resources');
  const destination = path.join(resources, WINFSP_INSTALLER);
  await mkdir(resources, { recursive: true });

  try {
    if (sha256(await readFile(destination)) === WINFSP_SHA256) return destination;
  } catch {
    // Missing or stale installer: replace it from the pinned release below.
  }

  const response = await fetch(
    `https://github.com/winfsp/winfsp/releases/download/v2.1/${WINFSP_INSTALLER}`
  );
  if (!response.ok)
    throw new Error(`Failed to download ${WINFSP_INSTALLER}: HTTP ${response.status}`);
  const installer = Buffer.from(await response.arrayBuffer());
  if (sha256(installer) !== WINFSP_SHA256)
    throw new Error(`SHA-256 mismatch for ${WINFSP_INSTALLER}`);

  const staged = `${destination}.tmp`;
  await writeFile(staged, installer);
  await rm(destination, { force: true });
  await rename(staged, destination);
  return destination;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  prepareWinFsp().then((destination) => {
    if (destination) console.log(`Prepared ${destination}`);
  });
}
