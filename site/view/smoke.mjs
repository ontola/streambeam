// Actual fixture -> native pinned QUIC -> WebTransport -> browser H.264 decode.
// No real desktop is captured; no invitation, pairing payload or browser URL is logged.
import { chromium } from 'playwright';
import { spawn, execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, readFile, writeFile, rm, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../..');
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {cwd: root}));
const bin = join(metadata.target_directory, 'debug');
const directory = await mkdtemp(join(tmpdir(), 'streambeam-web-'));
const children = [];
let browser;
const server = createServer(async (req, res) => {
  // Exact static-file allowlist; no invitation can become a request URL.
  const files = {'/view/':'view/index.html','/view/viewer.js':'view/viewer.js','/view/controls.js':'view/controls.js','/view/viewer.css':'view/viewer.css','/assets/streambeam-logo.svg':'assets/streambeam-logo.svg'};
  const name = files[req.url];
  if (!name) {res.writeHead(404).end(); return;}
  const mime = name.endsWith('.js') ? 'text/javascript' : name.endsWith('.css') ? 'text/css' : name.endsWith('.svg') ? 'image/svg+xml' : 'text/html';
  res.writeHead(200, {'content-type':mime,'cache-control':'no-store'});
  res.end(await readFile(join(root, 'site', name)));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
function child(name, args, env = {}) {
  const process = spawn(join(bin, name), args, {cwd:root, env:{...globalThis.process.env,...env}, stdio:['ignore','pipe','pipe']});
  children.push(process);
  return process;
}
async function until(condition, message) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) { const result = await condition(); if (result) return result; await new Promise(resolve => setTimeout(resolve, 50)); }
  throw new Error(message);
}
try {
  const inputFile = join(directory,'input.jsonl');
  const host = child('fakehost', ['--input-events-file',inputFile], {STREAMBEAM_LISTEN:'127.0.0.1:0',STREAMBEAM_NO_MDNS:'1'});
  let output = '';
  host.stdout.on('data', data => output += data);
  const payload = await until(() => output.split('\n').find(line => line.startsWith('sbm1:')), 'Fixture did not start');
  const pairFile = join(directory, 'pair');
  const inviteFile = join(directory, 'invite');
  await writeFile(pairFile, payload, {mode:0o600});
  const bridge = child('webbridge', ['--pair-file',pairFile,'--invite-file',inviteFile,'--viewer-url',`${origin}/view/`,'--listen','127.0.0.1:0']);
  const invitation = await until(async () => {try{return (await readFile(inviteFile,'utf8')).trim()}catch{return null}}, 'Bridge did not write invitation');
  browser = await chromium.launch({channel:process.env.STREAMBEAM_BROWSER_CHANNEL || 'chrome',headless:true});
  const page = await browser.newPage();
  // A wrong capability must not consume the invitation or receive any config/video.
  const invalid = new URL(invitation);
  const fragment = new URLSearchParams(invalid.hash.slice(1));
  fragment.set('token','00'.repeat(32)); invalid.hash = fragment.toString();
  await page.goto(invalid.href);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#stop').disabled);
  assert.equal(await page.locator('#screen').getAttribute('data-decoded-frames'), null);
  assert.equal(new URL(page.url()).hash, '');
  assert.equal(bridge.exitCode,null);
  // A different browser origin is refused even with the correct capability.
  const wrongOrigin = new URL(invitation); wrongOrigin.hostname = 'localhost';
  await page.goto('about:blank');
  await page.goto(wrongOrigin.href);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#stop').disabled);
  assert.equal(await page.locator('#screen').getAttribute('data-decoded-frames'), null);
  assert.equal(bridge.exitCode,null);
  // Certificate pinning must reject a server when the invitation fingerprint differs.
  const wrongCert = new URL(invitation);
  const certFragment = new URLSearchParams(wrongCert.hash.slice(1));
  certFragment.set('cert','00'.repeat(32)); wrongCert.hash = certFragment.toString();
  await page.goto('about:blank');
  await page.goto(wrongCert.href);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#stop').disabled);
  assert.equal(await page.locator('#screen').getAttribute('data-decoded-frames'), null);
  assert.equal(bridge.exitCode,null);
  // Fresh authenticated page must decode the genuine native fixture, not mock JS frames.
  await page.goto('about:blank');
  await page.goto(invitation);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.waitForFunction(() => Number(document.querySelector('#screen').dataset.decodedFrames) >= 30, null, {timeout:30000});
  const evidence = await page.locator('#screen').evaluate(canvas => {
    const pixels = canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
    const colors = new Set();
    for(let n=0;n<pixels.length;n+=400) colors.add(`${pixels[n]},${pixels[n+1]},${pixels[n+2]}`);
    return {width:canvas.width,height:canvas.height,frames:Number(canvas.dataset.decodedFrames),colors:colors.size};
  });
  assert.equal(evidence.width,640); assert.equal(evidence.height,360); assert.ok(evidence.colors>20);
  const recorded = async () => (await readFile(inputFile,'utf8')).split('\n').slice(0,-1).filter(Boolean).map(line=>JSON.parse(line));
  const hasKey=(events,vk,down)=>events.some(e=>e.Key?.vk===vk && e.Key.down===down);
  const hasButton=(events,down)=>events.some(e=>e.MouseButton?.button===0 && e.MouseButton.down===down);
  // Viewing alone must never forward pointer or keyboard events.
  await page.locator('#screen').click({position:{x:100,y:80}});
  await page.keyboard.press('a');
  await page.waitForFunction(() => Number(document.querySelector('#screen').dataset.decodedFrames) >= 40);
  assert.equal((await recorded()).length,0);
  await page.locator('#control').check();
  await page.keyboard.down('Shift'); await page.keyboard.down('a');
  const bounds=await page.locator('#screen').boundingBox();
  await page.mouse.move(bounds.x+100,bounds.y+80); await page.mouse.down();
  await until(async()=>{const e=await recorded();return hasKey(e,65,true)&&hasKey(e,160,true)&&hasButton(e,true);},'Native fixture did not receive browser input');
  await page.locator('#stop').focus();
  await until(async()=>{const e=await recorded();return hasKey(e,65,false)&&hasKey(e,160,false)&&hasButton(e,false);},'Focus loss did not release held input');
  assert.equal(await page.locator('#control').isChecked(),false);
  await page.keyboard.up('a');await page.keyboard.up('Shift');await page.mouse.up();
  // Closing the actual browser tab with held keys must also release native input.
  await page.locator('#control').check();
  await page.keyboard.down('b');
  await until(async()=>hasKey(await recorded(),66,true),'Second control session did not start');
  const artifactDir = resolve(process.env.STREAMBEAM_WEB_ARTIFACTS || join(metadata.target_directory,'web-smoke'));
  await mkdir(artifactDir,{recursive:true});
  await page.screenshot({path:join(artifactDir,'browser-viewer.png'),fullPage:true});
  await page.close();
  await until(() => bridge.exitCode !== null, 'Bridge did not release native session after tab close');
  assert.ok(hasKey(await recorded(),66,false),'Tab close did not release held B');
  assert.equal(bridge.exitCode,0);
  assert.equal(await readFile(inviteFile).then(()=>true,()=>false),false);
  console.log(JSON.stringify({result:'passed',...evidence,wrongTokenRejected:true,wrongOriginRejected:true,wrongCertificateRejected:true,fragmentCleared:true,inputConsentVerified:true,nativeInputReceived:true,focusLossReleased:true,tabCloseReleased:true,cleanShutdown:true,screenshot:join(artifactDir,'browser-viewer.png')}));
} finally {
  await browser?.close();
  for (const process of children) process.kill('SIGTERM');
  await new Promise(resolve => server.close(resolve));
  await rm(directory,{recursive:true,force:true});
}
