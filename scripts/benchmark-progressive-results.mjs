import { performance } from 'node:perf_hooks';

const batchSize = 128;
const counts = (process.argv[2] ?? '10000,100000')
  .split(',')
  .map(Number)
  .filter((value) => Number.isInteger(value) && value > 0);

function entries(count) {
  return Array.from({ length: count }, (_, index) => ({
    name: `file-${String(count - index).padStart(7, '0')}.txt`,
    hidden: index % 31 === 0,
    isDirectory: index % 23 === 0,
  }));
}

function visible(entry) {
  return !entry.hidden && entry.name.includes('file-');
}

function sort(items) {
  items.sort((left, right) => {
    if (left.isDirectory !== right.isDirectory) return left.isDirectory ? -1 : 1;
    return left.name.localeCompare(right.name);
  });
}

function legacy(input) {
  const accumulated = [];
  let output = [];
  for (let offset = 0; offset < input.length; offset += batchSize) {
    accumulated.push(...input.slice(offset, offset + batchSize));
    output = accumulated.filter(visible);
    sort(output);
  }
  return output;
}

function progressive(input) {
  const output = [];
  for (let offset = 0; offset < input.length; offset += batchSize) {
    output.push(...input.slice(offset, offset + batchSize).filter(visible));
  }
  sort(output);
  return output;
}

for (const count of counts) {
  const input = entries(count);
  const legacyStart = performance.now();
  const legacyOutput = legacy(input);
  const legacyMs = performance.now() - legacyStart;
  const progressiveStart = performance.now();
  const progressiveOutput = progressive(input);
  const progressiveMs = performance.now() - progressiveStart;
  if (
    legacyOutput.length !== progressiveOutput.length ||
    legacyOutput[0]?.name !== progressiveOutput[0]?.name ||
    legacyOutput.at(-1)?.name !== progressiveOutput.at(-1)?.name
  ) {
    throw new Error(`Result mismatch at ${count} entries`);
  }
  console.log(JSON.stringify({
    entries: count,
    batchSize,
    visibleEntries: progressiveOutput.length,
    legacyMs: Number(legacyMs.toFixed(2)),
    progressiveMs: Number(progressiveMs.toFixed(2)),
    speedup: Number((legacyMs / progressiveMs).toFixed(2)),
    rssMiB: Number((process.memoryUsage().rss / 1024 / 1024).toFixed(1)),
  }));
}
