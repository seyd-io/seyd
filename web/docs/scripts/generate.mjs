// Generate everything the docs site takes from the code (PLAN.md §2.8).
// Runs before `astro build` and `astro dev`. Every output path is gitignored.
//
//   node scripts/generate.mjs                 # all of it
//   SEYD_DOCS_SKIP_RUSTDOC=1 …               # skip `cargo doc` (slow; the Rust page then links nowhere locally)
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../../..');
const content = resolve(here, '../src/content/docs');
const tools = join(root, 'tools/docs');

function run(cmd, args, opts = {}) {
  execFileSync(cmd, args, { stdio: 'inherit', cwd: root, ...opts });
}

// 1. References rendered from the code by the Python generators.
for (const script of ['gen-c-reference.py', 'gen-python-reference.py', 'gen-seydd-config.py', 'gen-qos-profiles.py']) {
  run('python3', [join(tools, script)]);
}

// 2. Repository documents that are the source of truth for their subject:
//    copied in with front matter derived from the first heading, so the site
//    is a view of docs/ rather than a second copy of it.
function importDoc(src, dest, { title, description, order } = {}) {
  const text = readFileSync(join(root, src), 'utf8');
  const m = text.match(/^# (.+)$/m);
  const heading = title ?? (m ? m[1].trim() : basename(src, '.md'));
  const body = m ? text.replace(m[0], '').trimStart() : text;
  const fm = [`title: ${JSON.stringify(heading)}`];
  if (description) fm.push(`description: ${JSON.stringify(description)}`);
  if (order !== undefined) fm.push(`sidebar:\n  order: ${order}`);
  const out = join(content, dest);
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, `---\n${fm.join('\n')}\n---\n\n:::note[Source]\nThis page is \`${src}\` in the repository, rendered as is. Change it there.\n:::\n\n${body}`);
}

rmSync(join(content, 'reference/protocol'), { recursive: true, force: true });
rmSync(join(content, 'reference/adr'), { recursive: true, force: true });
const protocolOrder = ['chunks', 'control-stream', 'signal-v2', 'seydd'];
for (const f of readdirSync(join(root, 'docs/protocol')).filter((f) => f.endsWith('.md') && f !== 'README.md')) {
  const slug = basename(f, '.md');
  importDoc(`docs/protocol/${f}`, `reference/protocol/${f}`, { order: protocolOrder.indexOf(slug) + 1 || 99 });
}
for (const f of readdirSync(join(root, 'docs/adr')).filter((f) => f.endsWith('.md'))) {
  importDoc(`docs/adr/${f}`, `reference/adr/${f}`, { order: parseInt(f, 10) });
}
importDoc('docs/encoder-setup.md', 'robot/encoder-setup.md', { description: 'How a publisher (x264, GStreamer, NVENC, Jetson, Hikvision, Axis, ONVIF) must be configured for Seyd, and how to verify it.' });
importDoc('docs/starlink.md', 'networking/starlink.md', { description: 'Making a Seyd robot reachable over Starlink, and what the link gives you.' });
importDoc('docs/self-hosting-auth.md', 'cloud/identity-providers.md', { title: 'Identity providers', description: 'Bundled Logto, bring your own OIDC provider, or headless: what a self-hosted Seyd delegates and what it never does.' });

// 3. The networking pages must cover every failure class the SDK links to.
run('python3', [join(tools, 'check-networking.py')]);

// 3b. The integration skill (skills/seyd/, the document a developer hands
//     their coding agent) must mention every public surface and name only
//     paths and routes that exist; its two generated references must be
//     current. Then it is published verbatim at /docs/skill/ for download.
run('python3', [join(tools, 'gen-skill.py'), '--check']);
const skillOut = resolve(here, '../public/skill');
rmSync(skillOut, { recursive: true, force: true });
cpSync(join(root, 'skills/seyd'), skillOut, { recursive: true });
// files.txt lets a shell one-liner on the "Integrate with a coding agent" page fetch the whole skill.
const skillFiles = readdirSync(skillOut, { recursive: true }).filter((f) => f.endsWith('.md')).sort();
writeFileSync(join(skillOut, 'files.txt'), skillFiles.join('\n') + '\n');
console.log(`generate: skills/seyd → web/docs/public/skill (${skillFiles.length} files)`);

// 4. TypeScript examples are type-checked, so a guide that imports one cannot show stale code.
run(resolve(here, '../node_modules/.bin/tsc'), ['--noEmit', '-p', join(root, 'sdks/js/core/examples/tsconfig.json')]);

// 5. rustdoc for every workspace crate, served at /docs/rust/.
const rustOut = resolve(here, '../public/rust');
if (process.env.SEYD_DOCS_SKIP_RUSTDOC) {
  console.log('generate: skipping rustdoc (SEYD_DOCS_SKIP_RUSTDOC set)');
} else {
  const cargo = process.env.CARGO ?? join(process.env.HOME ?? '', '.cargo/bin/cargo');
  run(existsSync(cargo) ? cargo : 'cargo', ['doc', '--workspace', '--no-deps', '--quiet']);
  rmSync(rustOut, { recursive: true, force: true });
  cpSync(join(root, 'target/doc'), rustOut, { recursive: true });
  console.log('generate: rustdoc → web/docs/public/rust');
}
