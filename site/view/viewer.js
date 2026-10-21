import { audioPlayback } from './audio.js';
import { attachControls } from './controls.js';
const status = document.querySelector('#status');
const start = document.querySelector('#connect');
const stop = document.querySelector('#stop');
const canvas = document.querySelector('#screen');
const stats = document.querySelector('#stats');
const context = canvas.getContext('2d');
const MAX_PACKET = 4 * 1024 * 1024;
let transport, decoder, controller, sound, stopped = false;

function fromHex(value) {
  if (!/^[a-f0-9]{64}$/.test(value ?? '')) throw new Error('Invalid invitation.');
  return Uint8Array.from(value.match(/../g), byte => parseInt(byte, 16));
}
function invitation(fragment) {
  const p = new URLSearchParams(fragment);
  if (p.get('v') !== '1') throw new Error('Open a fresh browser bridge invitation.');
  const endpoint = new URL(p.get('endpoint'));
  if (endpoint.protocol !== 'https:' || endpoint.pathname !== '/view' || endpoint.search || endpoint.hash || endpoint.username || endpoint.password) throw new Error('Invalid bridge address.');
  return { endpoint: endpoint.href, cert: fromHex(p.get('cert')), token: fromHex(p.get('token')) };
}
// The capability never becomes an HTTP request, storage entry or telemetry event.
window.addEventListener('hashchange', () => { if (location.hash) location.reload(); });
let invite;
try {
  const fragment = location.hash.slice(1);
  history.replaceState(null, '', location.pathname);
  invite = invitation(fragment);
  if (!isSecureContext || !('WebTransport' in window) || !('VideoDecoder' in window)) throw new Error('This preview needs a browser with WebTransport and WebCodecs. Try a current Chrome or Edge browser.');
  status.textContent = 'Your private invitation is ready. Connect within five minutes.';
  start.disabled = false;
} catch (error) { status.textContent = error.message; }

function end(message) {
  stopped = true;
  controller?.dispose();
  sound?.dispose();
  transport?.close();
  if (decoder && decoder.state !== 'closed') decoder.close();
  stop.disabled = true;
  status.textContent = message;
}
stop.onclick = () => end('Stopped. Open a new invitation to connect again.');
window.addEventListener('pagehide', () => end('Stopped.'));

// Bounded stream parser: never buffer more than one capped encoded frame.
async function* packets(readable) {
  const reader = readable.getReader();
  let chunk = new Uint8Array(0), offset = 0;
  async function read(length) {
    const result = new Uint8Array(length);
    let written = 0;
    while (written < length) {
      if (offset === chunk.length) {
        const next = await reader.read();
        if (next.done) throw new Error('Stream ended.');
        chunk = next.value; offset = 0;
      }
      const count = Math.min(length - written, chunk.length - offset);
      result.set(chunk.subarray(offset, offset + count), written);
      offset += count; written += count;
    }
    return result;
  }
  try {
    while (!stopped) {
      const size = new DataView((await read(4)).buffer).getUint32(0);
      if (size === 0 || size > MAX_PACKET) throw new Error('Invalid video packet.');
      yield await read(size);
    }
  } finally { reader.releaseLock(); }
}

// Keep reference frames intact while applying bounded backpressure. A temporary
// main-thread pause can deliver a burst of packets even on a fast decoder.
async function decoderReady() {
  if (decoder.decodeQueueSize < 8) return;
  await new Promise((resolve, reject) => {
    const cleanup = () => { clearTimeout(timer); decoder.removeEventListener('dequeue', ready); };
    const ready = () => {
      if (stopped || decoder.state !== 'configured') {
        cleanup(); reject(new Error('Video decoding stopped.'));
      } else if (decoder.decodeQueueSize < 8) {
        cleanup(); resolve();
      }
    };
    const timer = setTimeout(() => {
      cleanup(); reject(new Error('The browser cannot keep up with this stream. Try the native app.'));
    }, 2000);
    decoder.addEventListener('dequeue', ready);
    ready();
  });
}

start.onclick = async () => {
  start.disabled = true;
  stop.disabled = false;
  status.textContent = 'Connecting securely…';
  try {
    transport = new WebTransport(invite.endpoint, { serverCertificateHashes: [{ algorithm: 'sha-256', value: invite.cert }] });
    transport.closed.catch(() => { if (!stopped) end('Connection closed. Open a fresh invitation and try again.'); });
    await transport.ready;
    const stream = await transport.createBidirectionalStream();
    const writer = stream.writable.getWriter();
    await writer.write(invite.token);
    invite.token.fill(0);
    let configured = false, frames = 0;
    for await (const packet of packets(stream.readable)) {
      if (!configured) {
        const metadata = JSON.parse(new TextDecoder().decode(packet));
        if (metadata.version !== 2 || !/^avc1\.[a-f0-9]{6}$/.test(metadata.codec) || metadata.width < 1 || metadata.width > 4096 || metadata.height < 1 || metadata.height > 4096) throw new Error('Unsupported video configuration.');
        const config = { codec: metadata.codec, codedWidth: metadata.width, codedHeight: metadata.height, optimizeForLatency: true };
        if (!(await VideoDecoder.isConfigSupported(config)).supported) throw new Error('This browser cannot decode the shared screen. Try the native StreamBeam app.');
        decoder = new VideoDecoder({
          output(frame) {
            try {
              canvas.width = frame.displayWidth; canvas.height = frame.displayHeight;
              context.drawImage(frame, 0, 0); canvas.hidden = false;
              frames += 1;
              status.textContent = 'Connected';
              stats.textContent = `${canvas.width} × ${canvas.height} · ${frames} decoded frames`;
              canvas.dataset.decodedFrames = String(frames);
            } finally { frame.close(); }
          },
          error() { end('Video decoding stopped. Open a new invitation to retry.'); }
        });
        decoder.configure(config); configured = true;
        controller = attachControls({canvas,writer,allowed:metadata.control===true,onError:end});
        sound = await audioPlayback(metadata.audio);
      } else {
        if (packet.length < 10 || packet[0] > 2) throw new Error('Invalid media frame.');
        if (packet[0] === 2) { sound?.push(packet); continue; }
        await decoderReady();
        if (stopped) break;
        decoder.decode(new EncodedVideoChunk({ type: packet[0] ? 'key' : 'delta', timestamp: Number(new DataView(packet.buffer, packet.byteOffset + 1, 8).getBigUint64(0)), data: packet.subarray(9) }));
      }
    }
  } catch (error) {
    if (!stopped) end(error.message.startsWith('WebTransport') ? 'Could not reach the bridge. Check its address, invitation expiry and local network access.' : error.message);
  }
};
