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
let browser, page;
let phase = "startup";
const artifactDir = resolve(process.env.STREAMBEAM_WEB_ARTIFACTS || join(metadata.target_directory,"web-smoke"));
const server = createServer(async (req, res) => {
  // Exact static-file allowlist; no invitation can become a request URL.
  const files = {'/view/':'view/index.html','/view/viewer.js':'view/viewer.js','/view/controls.js':'view/controls.js','/view/audio.js':'view/audio.js','/view/viewer.css':'view/viewer.css','/assets/streambeam-logo.svg':'assets/streambeam-logo.svg'};
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
  process.stderr.resume(); // Drain without retaining potentially private fixture logs.
  children.push(process);
  return process;
}
async function until(condition, message) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) { const result = await condition(); if (result) return result; await new Promise(resolve => setTimeout(resolve, 50)); }
  throw new Error(message);
}
async function decodedFrames(count) {
  await page.waitForFunction(target => document.querySelector('#stop').disabled || Number(document.querySelector('#screen').dataset.decodedFrames) >= target, count, {timeout:30000});
  assert.ok(Number(await page.locator('#screen').getAttribute('data-decoded-frames')) >= count, 'Stream ended before enough frames were decoded');
}

try {
  await mkdir(artifactDir,{recursive:true});
  for (const name of ['browser-viewer.png','browser-failure.png','failure.json']) await rm(join(artifactDir,name),{force:true});
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
  page = await browser.newPage();
  phase = "invalid-capability";
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
  phase = "invalid-origin";
  // A different browser origin is refused even with the correct capability.
  const wrongOrigin = new URL(invitation); wrongOrigin.hostname = 'localhost';
  await page.goto('about:blank');
  await page.goto(wrongOrigin.href);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('#stop').disabled);
  assert.equal(await page.locator('#screen').getAttribute('data-decoded-frames'), null);
  assert.equal(bridge.exitCode,null);
  phase = "invalid-certificate";
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
  phase = "authenticated-video";
  // Fresh authenticated page must decode the genuine native fixture, not mock JS frames.
  await page.goto('about:blank');
  await page.goto(invitation);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await decodedFrames(30);
  phase = "video-stall-recovery";
  const framesBeforeStall = Number(await page.locator('#screen').getAttribute('data-decoded-frames'));
  await page.evaluate(()=>{const until=performance.now()+750;while(performance.now()<until){}});
  await decodedFrames(framesBeforeStall + 30);
  const evidence = await page.locator('#screen').evaluate(canvas => {
    const pixels = canvas.getContext('2d').getImageData(0,0,canvas.width,canvas.height).data;
    const colors = new Set();
    for(let n=0;n<pixels.length;n+=400) colors.add(`${pixels[n]},${pixels[n+1]},${pixels[n+2]}`);
    return {width:canvas.width,height:canvas.height,frames:Number(canvas.dataset.decodedFrames),colors:colors.size};
  });
  assert.equal(evidence.width,640); assert.equal(evidence.height,360); assert.ok(evidence.colors>20);
  phase = 'audio';
  assert.equal(await page.locator('#sound-status').getAttribute('data-decoded-frames'),null);
  await page.getByRole('button',{name:'Enable sound',exact:true}).click();
  // Simulate a short main-thread stall; queued network packets must recover.
  await page.evaluate(()=>{const until=performance.now()+250;while(performance.now()<until){}});
  await page.waitForFunction(()=>Number(document.querySelector('#sound-status').dataset.decodedFrames)>=4800).catch(async()=>{throw new Error('Audio verification failed: '+await page.locator('#sound-status').textContent()+'; '+await page.locator('#status').textContent());});
  const audioEvidence=await page.locator('#sound-status').evaluate(el=>({frames:Number(el.dataset.decodedFrames),peak:Number(el.dataset.peak),activeSources:Number(el.dataset.activeSources),droppedPackets:Number(el.dataset.droppedPackets||0)}));
  assert.ok(audioEvidence.peak>0.01);assert.ok(audioEvidence.activeSources<=24);
  await page.getByRole('button',{name:'Mute',exact:true}).click();
  assert.equal(await page.locator('#sound-status').getAttribute('data-enabled'),'false');
  assert.equal(await page.locator('#sound-status').getAttribute('data-active-sources'),'0');
  phase = 'input';
  const recorded = async () => (await readFile(inputFile,'utf8')).split('\n').slice(0,-1).filter(Boolean).map(line=>JSON.parse(line));
  const hasKey=(events,vk,down)=>events.some(e=>e.Key?.vk===vk && e.Key.down===down);
  const hasButton=(events,down)=>events.some(e=>e.MouseButton?.button===0 && e.MouseButton.down===down);
  // Viewing alone must never forward pointer or keyboard events.
  await page.locator('#screen').click({position:{x:100,y:80}});
  await page.keyboard.press('a');
  await decodedFrames(40);
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
  await mkdir(artifactDir,{recursive:true});
  await page.screenshot({path:join(artifactDir,'browser-viewer.png'),fullPage:true});
  await page.close();
  await until(() => bridge.exitCode !== null, 'Bridge did not release native session after tab close');
  assert.ok(hasKey(await recorded(),66,false),'Tab close did not release held B');
  assert.equal(bridge.exitCode,0);
  assert.equal(await readFile(inviteFile).then(()=>true,()=>false),false);
  console.log(JSON.stringify({result:'passed',...evidence,wrongTokenRejected:true,wrongOriginRejected:true,wrongCertificateRejected:true,fragmentCleared:true,audioDecoded:audioEvidence,audioStartsOnClick:true,audioMuteClearsQueue:true,inputConsentVerified:true,nativeInputReceived:true,focusLossReleased:true,tabCloseReleased:true,cleanShutdown:true,screenshot:join(artifactDir,'browser-viewer.png')}));
} catch (error) {
  // Deliberately allowlist diagnostics: no URLs, console logs, request details,
  // child stdout/stderr or browser traces (all can contain private capabilities).
  await mkdir(artifactDir,{recursive:true});
  const state = page && !page.isClosed() ? await page.evaluate(() => ({
    status: document.querySelector('#status')?.textContent,
    stats: document.querySelector('#stats')?.textContent,
    sound: document.querySelector('#sound-status')?.textContent,
    decodedFrames: document.querySelector('#screen')?.dataset.decodedFrames,
    secureContext: isSecureContext,
    webTransport: 'WebTransport' in window,
    videoDecoder: 'VideoDecoder' in window,
    audioDecoder: 'AudioDecoder' in window,
  })).catch(()=>({unavailable:true})) : {unavailable:true};
  // Viewer errors are sanitized too, in case a future browser embeds an address.
  const clean = value => JSON.stringify(value).replace(/https?:[^\s"<>]+/g,'[address]').replace(/sbm1:[^\s"]+/g,'[pairing]').replace(/[a-fA-F0-9]{64}/g,'[secret]');
  const diagnostics = clean({phase,browser:browser?.version(),state,children:children.map(child=>({exitCode:child.exitCode,signal:child.signalCode})),errorType:error.name});
  await writeFile(join(artifactDir,'failure.json'),diagnostics+'\n');
  console.error(diagnostics);
  // Screenshots are safe only after invitation parsing has erased the fragment.
  // The page has no address bar; suppress arbitrary browser-generated errors.
  if (page && !page.isClosed() && new URL(page.url()).hash === '' && state.status) {
    await page.locator('#status').evaluate((el,text)=>{el.textContent=text;}, JSON.parse(clean(state.status)));
    await page.screenshot({path:join(artifactDir,'browser-failure.png'),fullPage:true}).catch(()=>{});
  }
  throw new Error(`Browser fixture failed during ${phase} (${error.name}); see sanitized failure.json`);
} finally {
  await browser?.close();
  for (const process of children) process.kill('SIGTERM');
  await new Promise(resolve => server.close(resolve));
  await rm(directory,{recursive:true,force:true});
}
