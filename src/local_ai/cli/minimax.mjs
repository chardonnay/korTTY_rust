import { registerHooks } from 'node:module';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

// Rust supplies a digest-pinned release and one bounded credential and message envelope.
const blocks = [];
let length = 0;
for await (const block of process.stdin) {
  length += block.length;
  if (length > 3 * 1024 * 1024) throw new Error('MiniMax input exceeds its limit');
  blocks.push(block);
}
const { module, key, messages, region, base, model, timeout } = JSON.parse(Buffer.concat(blocks));
const entry = pathToFileURL(resolve('kortty-mmx.mjs')).href;
const config = resolve('kortty-mmx-config', 'config.json');
const messageFile = '/dev/stdin';
const permittedEndpoint = `${base}/anthropic/v1/messages`;
if (!(region === 'global' && base === 'https://api.minimax.io' || region === 'cn' &&
    ['https://api.minimaxi.com', 'https://api.minimax.cn'].includes(base))) {
  throw new Error('Invalid MiniMax region');
}

const configText = JSON.stringify({ api_key: key, region });
const readVirtual = path => {
  if (path === config) return configText;
  // The older official package uses /dev/stdin, which is absent on Windows.
  if (path === messageFile || path === 0) return messages;
  throw new Error('MiniMax attempted to read an unreviewed path');
};
const deny = () => { throw new Error('MiniMax requested a disabled operation'); };
Object.defineProperty(globalThis, '__korttyMiniMaxFiles', {
  value: Object.freeze({ readVirtual, config, deny }),
});
const virtualFs = `
  const { readVirtual, config, deny } = globalThis.__korttyMiniMaxFiles;
  export const readFileSync = readVirtual;
  export const existsSync = path => path === config;
  export const constants = Object.freeze({});
  export const accessSync = deny, chmodSync = deny, closeSync = deny, copyFileSync = deny,
    createWriteStream = deny, lstatSync = deny, mkdirSync = deny, openSync = deny,
    realpathSync = deny, renameSync = deny, rmSync = deny, statSync = deny, unlinkSync = deny,
    writeFileSync = deny;
`;
const disabledFs = `
  export const readFile = globalThis.__korttyMiniMaxFiles.deny;
  export const mkdir = readFile;
`;
const disabledChild = `
  export const spawn = globalThis.__korttyMiniMaxFiles.deny;
  export const spawnSync = spawn;
`;
const disabledProxy = `
  export const setGlobalDispatcher = globalThis.__korttyMiniMaxFiles.deny;
  export const ProxyAgent = setGlobalDispatcher;
`;
const virtual = new Map([
  ['kortty:fs', virtualFs], ['kortty:fs/promises', disabledFs],
  ['kortty:child_process', disabledChild], ['kortty:undici', disabledProxy],
]);
const builtins = new Set(['node:buffer', 'node:crypto', 'node:module', 'node:os',
  'node:path', 'node:process', 'node:readline', 'node:tty']);

const fetchHttps = globalThis.fetch;
let dispatched = false;
Object.defineProperty(globalThis, 'fetch', {
  value(url, request) {
    if (dispatched || url !== permittedEndpoint || request?.method !== 'POST' ||
        new Headers(request.headers).get('x-api-key') !== key ||
        typeof request.body !== 'string') {
      throw new Error('MiniMax attempted an unreviewed request');
    }
    dispatched = true;
    return fetchHttps(url, { ...request, redirect: 'error' });
  },
  writable: false,
  configurable: false,
});
process.env.MMX_CONFIG_DIR = resolve('kortty-mmx-config');
process.env.CI = 'true';
process.argv = [process.execPath, 'mmx', 'text', 'chat', '--messages-file', '-',
  '--region', region, '--base-url', base, '--output', 'text', '--no-color', '--quiet',
  '--non-interactive', '--timeout', String(timeout)];
if (model) process.argv.push('--model', model);

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === entry && !context.parentURL?.startsWith(entry)) {
      return { url: entry, format: 'module', shortCircuit: true };
    }
    const name = specifier.replace(/^node:/, '');
    const mapped = `kortty:${name}`;
    if (virtual.has(mapped)) return { url: mapped, format: 'module', shortCircuit: true };
    if (builtins.has(`node:${name}`)) return nextResolve(`node:${name}`, context);
    return deny();
  },
  load(url, context, nextLoad) {
    const source = url === entry ? Buffer.from(module, 'base64') : virtual.get(url);
    if (source !== undefined) return { format: 'module', source, shortCircuit: true };
    if (builtins.has(url)) return nextLoad(url, context);
    return deny();
  },
});
await import(entry);
