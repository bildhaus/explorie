// Detached ed25519 signatures for update payloads, in minisign's prehashed
// ("ED") format so the updater can check them with the minisign-verify crate.
//
// The release contract allows exactly two assets, so signatures travel as
// machine-readable lines inside the release notes:
//
//   explorie-signature <asset-name> <signature-base64> <global-signature-base64>
//
// The two base64 fields are lines 2 and 4 of a minisign signature whose
// trusted comment is `explorie-update <asset-name>`. The updater rebuilds that
// comment from the asset it expects, so a signature cannot be replayed for a
// different (for example older) installer.
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  scryptSync,
  sign as ed25519Sign,
  verify as ed25519Verify,
} from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const PUBLIC_KEY_FILE = fileURLToPath(
  new URL('../crates/native-services/update-signing-key.pub', import.meta.url),
);
export const SIGNATURE_PREFIX = 'explorie-signature';
const BLOCK_START = '<!-- explorie-update-signatures';
const BLOCK_END = '-->';
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');
const SPKI_ED25519_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');
const SECRET_KEY_BYTES = 158;

export function trustedComment(assetName) {
  return `explorie-update ${assetName}`;
}

function keyLines(text) {
  return text
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(line => line && !line.startsWith('untrusted comment:'));
}

function strictBase64(value, label) {
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(value)) throw new Error(`${label} is not valid base64`);
  const bytes = Buffer.from(value, 'base64');
  if (bytes.toString('base64') !== value) throw new Error(`${label} is not canonical base64`);
  return bytes;
}

/** Parse a minisign public key file; returns null when no key is configured. */
export function parsePublicKey(text) {
  const lines = keyLines(text);
  if (lines.length === 0) return null;
  if (lines.length !== 1) throw new Error('The update public key file must contain exactly one key');
  const bytes = strictBase64(lines[0], 'The update public key');
  if (bytes.length !== 42 || bytes.subarray(0, 2).toString('latin1') !== 'Ed') {
    throw new Error('The update public key is not a minisign ed25519 key');
  }
  return { keyId: bytes.subarray(2, 10), key: bytes.subarray(10, 42) };
}

function publicKeyObject(key) {
  return createPublicKey({ key: Buffer.concat([SPKI_ED25519_PREFIX, key]), format: 'der', type: 'spki' });
}

// libsodium's crypto_pwhash_scryptsalsa208sha256 parameter selection, which
// minisign uses to encrypt secret keys.
export function scryptParameters(opslimit, memlimit) {
  let ops = BigInt(opslimit);
  const mem = BigInt(memlimit);
  if (ops < 32768n) ops = 32768n;
  const r = 8n;
  let logN = 1n;
  let p;
  if (ops < mem / 32n) {
    p = 1n;
    const maxN = ops / (r * 4n);
    for (; logN < 63n; logN += 1n) if (1n << logN > maxN / 2n) break;
  } else {
    const maxN = mem / (r * 128n);
    for (; logN < 63n; logN += 1n) if (1n << logN > maxN / 2n) break;
    let maxrp = ops / 4n / (1n << logN);
    if (maxrp > 0x3fffffffn) maxrp = 0x3fffffffn;
    p = maxrp / r;
  }
  const N = 1n << logN;
  if (N > 1n << 30n || p < 1n) throw new Error('The update signing key uses unsupported scrypt limits');
  return { N: Number(N), r: Number(r), p: Number(p), maxmem: Number(256n * N * r + (64n << 20n)) };
}

/** Parse (and decrypt, if needed) a minisign secret key file. */
export function parseSecretKey(text, password = '') {
  const lines = keyLines(text);
  if (lines.length !== 1) throw new Error('The update signing key must be a minisign secret key file');
  const bytes = strictBase64(lines[0], 'The update signing key');
  if (bytes.length !== SECRET_KEY_BYTES || bytes.subarray(0, 2).toString('latin1') !== 'Ed') {
    throw new Error('The update signing key is not a minisign ed25519 secret key');
  }
  const kdf = bytes.subarray(2, 4);
  const keynum = Buffer.from(bytes.subarray(54, SECRET_KEY_BYTES));
  if (kdf.toString('latin1') === 'Sc') {
    // Keys generated without a password may still be encrypted with an empty one.
    const stream = scryptSync(
      Buffer.from(password, 'utf8'),
      bytes.subarray(6, 38),
      keynum.length,
      scryptParameters(bytes.readBigUInt64LE(38), bytes.readBigUInt64LE(46)),
    );
    for (let index = 0; index < keynum.length; index += 1) keynum[index] ^= stream[index];
  } else if (kdf.readUInt16LE(0) !== 0) {
    throw new Error('The update signing key uses an unsupported key derivation');
  }
  const keyId = keynum.subarray(0, 8);
  const seed = keynum.subarray(8, 40);
  const key = keynum.subarray(40, 72);
  const privateKey = createPrivateKey({
    key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]),
    format: 'der',
    type: 'pkcs8',
  });
  const derived = createPublicKey(privateKey).export({ format: 'der', type: 'spki' }).subarray(12);
  if (!derived.equals(key)) {
    throw new Error('Wrong UPDATE_SIGNING_KEY_PASSWORD for the update signing key, or a corrupt key');
  }
  return { keyId, key, privateKey };
}

async function blake2b512(file) {
  const hash = createHash('blake2b512');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest();
}

/** Sign one asset file; returns its release-notes line. */
export async function signAsset(file, assetName, secret) {
  if (!/^[\w.-]+$/.test(assetName)) throw new Error(`Unsupported asset name: ${assetName}`);
  const signature = ed25519Sign(null, await blake2b512(file), secret.privateKey);
  const global = ed25519Sign(
    null,
    Buffer.concat([signature, Buffer.from(trustedComment(assetName), 'utf8')]),
    secret.privateKey,
  );
  const encoded = Buffer.concat([Buffer.from('ED', 'latin1'), secret.keyId, signature]).toString('base64');
  return `${SIGNATURE_PREFIX} ${assetName} ${encoded} ${global.toString('base64')}`;
}

/** Collect signature lines from release notes, keyed by asset name. */
export function parseSignatureLines(notes) {
  const signatures = new Map();
  for (const line of (notes ?? '').split(/\r?\n/)) {
    const fields = line.trim().split(/\s+/);
    if (fields[0] !== SIGNATURE_PREFIX) continue;
    if (fields.length !== 4) throw new Error('Malformed update signature line');
    const [, assetName, signature, global] = fields;
    if (signatures.has(assetName)) throw new Error(`Duplicate update signature for ${assetName}`);
    signatures.set(assetName, { signature, global });
  }
  return signatures;
}

/** Verify an asset against its signature line with the configured public key. */
export async function verifyAsset(file, assetName, entry, publicKey) {
  if (!entry) throw new Error(`Missing update signature for ${assetName}`);
  const signature = strictBase64(entry.signature, 'The update signature');
  const global = strictBase64(entry.global, 'The update global signature');
  if (signature.length !== 74 || global.length !== 64 || signature.subarray(0, 2).toString('latin1') !== 'ED') {
    throw new Error(`Update signature for ${assetName} is not a prehashed minisign signature`);
  }
  if (!signature.subarray(2, 10).equals(publicKey.keyId)) {
    throw new Error(`Update signature for ${assetName} was made with a different key`);
  }
  const key = publicKeyObject(publicKey.key);
  const raw = signature.subarray(10);
  const comment = Buffer.from(trustedComment(assetName), 'utf8');
  if (!ed25519Verify(null, await blake2b512(file), key, raw)
      || !ed25519Verify(null, Buffer.concat([raw, comment]), key, global)) {
    throw new Error(`Update signature for ${assetName} is invalid`);
  }
}

export function stripSignatureBlock(notes) {
  const kept = [];
  let inside = false;
  for (const line of (notes ?? '').split(/\r?\n/)) {
    if (!inside && line.trim() === BLOCK_START) {
      inside = true;
      continue;
    }
    if (inside) {
      if (line.trim() === BLOCK_END) inside = false;
      continue;
    }
    if (line.trim().startsWith(`${SIGNATURE_PREFIX} `)) continue;
    kept.push(line);
  }
  return kept.join('\n').trimEnd();
}

/** Replace any previous signature block in the notes with `lines`. */
export function withSignatureBlock(notes, lines) {
  const body = stripSignatureBlock(notes);
  return `${body}${body ? '\n\n' : ''}${BLOCK_START}\n${lines.join('\n')}\n${BLOCK_END}\n`;
}

export async function loadPublicKey(file = PUBLIC_KEY_FILE) {
  return parsePublicKey(await readFile(file, 'utf8'));
}

async function signCommand({ publicKeyFile, notesFile, assets, env }) {
  const publicKey = await loadPublicKey(publicKeyFile);
  const secretText = env.UPDATE_SIGNING_KEY ?? '';
  if (!publicKey && !secretText.trim()) {
    return 'Update signing is not configured; release notes were left unchanged.';
  }
  if (!publicKey) {
    throw new Error(`UPDATE_SIGNING_KEY is set but ${path.basename(publicKeyFile)} has no public key`);
  }
  if (!secretText.trim()) {
    throw new Error('An update public key is configured, so releases require the UPDATE_SIGNING_KEY secret');
  }
  const secret = parseSecretKey(secretText, env.UPDATE_SIGNING_KEY_PASSWORD ?? '');
  if (!secret.keyId.equals(publicKey.keyId) || !secret.key.equals(publicKey.key)) {
    throw new Error('UPDATE_SIGNING_KEY does not match the configured update public key');
  }
  const lines = [];
  for (const asset of assets) {
    const name = path.basename(asset);
    const line = await signAsset(asset, name, secret);
    await verifyAsset(asset, name, parseSignatureLines(line).get(name), publicKey);
    lines.push(line);
  }
  const notes = await readFile(notesFile, 'utf8');
  await writeFile(notesFile, withSignatureBlock(notes, lines));
  return `Signed ${lines.length} update payload(s) into ${path.basename(notesFile)}`;
}

async function verifyCommand({ publicKeyFile, notesFile, assets }) {
  const publicKey = await loadPublicKey(publicKeyFile);
  if (!publicKey) return 'Update signing is not configured; nothing to verify.';
  const signatures = parseSignatureLines(await readFile(notesFile, 'utf8'));
  for (const asset of assets) {
    const name = path.basename(asset);
    await verifyAsset(asset, name, signatures.get(name), publicKey);
  }
  return `Verified update signatures for ${assets.length} payload(s)`;
}

export async function main(argv, env = process.env) {
  const [command, ...rest] = argv;
  let publicKeyFile = PUBLIC_KEY_FILE;
  let notesFile;
  const assets = [];
  for (let index = 0; index < rest.length; index += 1) {
    if (rest[index] === '--public-key') publicKeyFile = rest[++index];
    else if (rest[index] === '--notes') notesFile = rest[++index];
    else assets.push(rest[index]);
  }
  if (!['sign', 'verify'].includes(command) || !notesFile || assets.length === 0) {
    throw new Error('Usage: update-signatures.mjs sign|verify --notes <file> [--public-key <file>] <asset>...');
  }
  const options = { publicKeyFile, notesFile, assets, env };
  return command === 'sign' ? signCommand(options) : verifyCommand(options);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  console.log(await main(process.argv.slice(2)));
}
