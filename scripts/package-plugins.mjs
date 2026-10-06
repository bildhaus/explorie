import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, writeFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

export const PLUGINS = ['syncthing', 'git', 'obsidian'];
export const TARGETS = ['x86_64-pc-windows-msvc', 'aarch64-apple-darwin'];
const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

export function nativeTarget(platform = process.platform, architecture = process.arch) {
  if (platform === 'win32' && architecture === 'x64') return TARGETS[0];
  if (platform === 'darwin' && architecture === 'arm64') return TARGETS[1];
  throw new Error(`No official plugin packages for ${platform}/${architecture}`);
}

export function validateManifest(manifest, id, version, target) {
  if (manifest.id !== id || manifest.version !== version || manifest.protocolVersion !== 1) {
    throw new Error(`Incompatible identity, version, or protocol in ${id}/plugin.json`);
  }
  if (!manifest.name || !manifest.description || !Array.isArray(manifest.capabilities)
      || !Array.isArray(manifest.dependencies) || !Array.isArray(manifest.settings)) {
    throw new Error(`Incomplete manifest for ${id}`);
  }
  const executable = `explorie-plugin-${id}${target.includes('windows') ? '.exe' : ''}`;
  if (manifest.executables?.[target] !== executable) throw new Error(`Invalid executable for ${id}/${target}`);
  for (const filename of Object.values(manifest.executables)) {
    if (typeof filename !== 'string' || !filename || /[/\\:]/.test(filename) || filename === '.' || filename === '..') {
      throw new Error('Executables must be package-root filenames');
    }
  }
  return executable;
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

// Stored ZIP entries preserve signed executable bytes and Unix executable mode.
// Only two known root filenames are emitted; no filesystem traversal or symlinks.
export function createZip(entries) {
  const local = [], central = [];
  let offset = 0;
  for (const { name, bytes, executable = false } of entries) {
    if (!name || /[/\\:]/.test(name) || name === '.' || name === '..') throw new Error('Unsafe ZIP filename');
    if (bytes.length > 0xffffffff) throw new Error('Plugin exceeds ZIP32 size limit');
    const filename = Buffer.from(name);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50); header.writeUInt16LE(20, 4);
    header.writeUInt16LE(0x800, 6); header.writeUInt16LE(33, 12);
    header.writeUInt32LE(crc32(bytes), 14);
    header.writeUInt32LE(bytes.length, 18); header.writeUInt32LE(bytes.length, 22);
    header.writeUInt16LE(filename.length, 26);
    local.push(header, filename, bytes);
    const directory = Buffer.alloc(46);
    directory.writeUInt32LE(0x02014b50); directory.writeUInt16LE(0x314, 4);
    directory.writeUInt16LE(20, 6); header.copy(directory, 8, 6, 28);
    directory.writeUInt32LE(((executable ? 0o100755 : 0o100644) * 65536) >>> 0, 38);
    directory.writeUInt32LE(offset, 42);
    central.push(directory, filename);
    offset += header.length + filename.length + bytes.length;
  }
  const directory = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10); end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, directory, end]);
}

export async function packagePlugins({ root = repository, target = nativeTarget(), binaryDirectory,
  outputDirectory, build = true } = {}) {
  if (!TARGETS.includes(target)) throw new Error(`Unsupported plugin target: ${target}`);
  const { version } = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
  if (!/^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$/.test(version)) throw new Error('Invalid application version');
  binaryDirectory ??= path.join(root, 'target', target, 'release');
  outputDirectory ??= path.join(root, 'release-artifacts', `plugins-${target}`);
  if (build) {
    const result = spawnSync('cargo', ['build', '--release', '--locked', '--target', target,
      ...PLUGINS.flatMap(id => ['-p', `explorie-plugin-${id}`])], { cwd: root, stdio: 'inherit' });
    if (result.error || result.status !== 0) throw new Error('Plugin release build failed', { cause: result.error });
  }
  // Validate every input before replacing any previous package/catalog.
  const packages = await Promise.all(PLUGINS.map(async id => {
    const manifest = JSON.parse(await readFile(path.join(root, 'plugins', id, 'plugin.json'), 'utf8'));
    const executable = validateManifest(manifest, id, version, target);
    const binaryPath = path.join(binaryDirectory, executable);
    if (!(await stat(binaryPath)).isFile()) throw new Error(`Missing executable: ${binaryPath}`);
    const bytes = await readFile(binaryPath);
    if (!bytes.length) throw new Error(`Empty executable: ${binaryPath}`);
    const archive = createZip([
      { name: 'plugin.json', bytes: Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`) },
      { name: executable, bytes, executable: true },
    ]);
    const filename = `explorie-plugin-${id}-${version}-${target}.zip`;
    return { filename, archive, entry: { manifest, target,
      assetUrl: `https://github.com/bildhaus/explorie/releases/download/v${version}/${filename}`,
      sha256: sha256(archive) } };
  }));
  await mkdir(outputDirectory, { recursive: true });
  for (const { filename, archive } of packages) await writeFile(path.join(outputDirectory, filename), archive);
  const catalogPath = path.join(outputDirectory, `explorie-plugin-catalog-${target}.json`);
  await writeFile(catalogPath, `${JSON.stringify(packages.map(p => p.entry), null, 2)}\n`);
  const checksums = packages.map(p => `${p.entry.sha256}  ${p.filename}`);
  checksums.push(`${sha256(await readFile(catalogPath))}  ${path.basename(catalogPath)}`);
  await writeFile(path.join(outputDirectory, `SHA256SUMS-plugins-${target}.txt`), `${checksums.join('\n')}\n`);
  return catalogPath;
}

export async function verifyCatalog(catalogPath, target, version, packageDirectory = path.dirname(catalogPath)) {
  const entries = JSON.parse(await readFile(catalogPath, 'utf8'));
  if (!Array.isArray(entries) || entries.length !== PLUGINS.length) throw new Error('Official catalog must contain all three plugins');
  for (const id of PLUGINS) {
    const entry = entries.find(item => item.manifest?.id === id);
    if (!entry || entry.target !== target) throw new Error(`Missing official ${id}/${target}`);
    validateManifest(entry.manifest, id, version, target);
    const filename = `explorie-plugin-${id}-${version}-${target}.zip`;
    if (entry.assetUrl !== `https://github.com/bildhaus/explorie/releases/download/v${version}/${filename}`
        || !/^[a-f0-9]{64}$/.test(entry.sha256)) throw new Error(`Invalid official catalog asset for ${id}`);
    if (sha256(await readFile(path.join(packageDirectory, filename))) !== entry.sha256) {
      throw new Error(`Plugin package integrity failed: ${id}`);
    }
  }
  return entries;
}

// Copy the notarized ZIP bytes unchanged. The application embeds the catalog;
// the bundle only needs the packages authenticated by that catalog.
export async function stagePlugins(catalogPath, target, version, destination) {
  const entries = await verifyCatalog(catalogPath, target, version);
  await mkdir(destination, { recursive: true });
  for (const { manifest } of entries) {
    const filename = `explorie-plugin-${manifest.id}-${version}-${target}.zip`;
    const source = path.join(path.dirname(catalogPath), filename);
    const output = path.join(destination, filename);
    if (path.resolve(source) !== path.resolve(output)) await copyFile(source, output);
  }
  await verifyCatalog(catalogPath, target, version, destination);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const option = name => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
    const target = option('--target') ?? nativeTarget();
    const catalog = option('--verify-catalog');
    if (catalog) {
      const { version } = JSON.parse(await readFile(path.join(repository, 'package.json'), 'utf8'));
      if (option('--stage-directory')) {
        await stagePlugins(catalog, target, version, option('--stage-directory'));
      } else await verifyCatalog(catalog, target, version, option('--verify-directory'));
      console.log('Official plugin catalog and packages verified.');
    } else console.log(await packagePlugins({ target, binaryDirectory: option('--binary-dir'),
      outputDirectory: option('--output'), build: !args.includes('--no-build') }));
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
