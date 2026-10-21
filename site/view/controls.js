// Desktop input is explicitly enabled per session and revoked on any focus loss.
export function attachControls({canvas, writer, allowed, onError}) {
  const checkbox = document.querySelector('#control');
  const hint = document.querySelector('#control-hint');
  let enabled = false, pending = 0, chain = Promise.resolve(), motion, animation = 0, disposed = false;
  checkbox.disabled = !allowed;
  hint.textContent = allowed ? 'Enable control, then use the shared screen. Escape or leaving the screen releases control.' : 'Remote control is currently supported for desktop hosts only.';
  function send(command) {
    if (disposed) return;
    if (pending >= 32) { onError('Remote input cannot keep up. The connection was stopped.'); return; }
    const body = new TextEncoder().encode(JSON.stringify(command));
    const packet = new Uint8Array(body.length + 2);
    new DataView(packet.buffer).setUint16(0, body.length); packet.set(body,2);
    pending++;
    chain = chain.then(() => writer.write(packet)).catch(() => { if (!disposed) onError('Remote control disconnected.'); }).finally(() => pending--);
  }
  function disable() {
    if (!enabled) return;
    enabled = false; checkbox.checked = false; motion = null;
    send({type:'control',enabled:false});
    hint.textContent = 'Control released. Enable it again when you are ready.';
  }
  checkbox.onchange = () => {
    if (!checkbox.checked) { disable(); return; }
    if (!allowed || disposed) return;
    enabled = true; send({type:'control',enabled:true});
    canvas.focus({preventScroll:true});
    hint.textContent = 'You control this desktop. Press Escape to release control.';
  };
  function position(event) {
    const rect = canvas.getBoundingClientRect();
    return {type:'move',x:Math.min(canvas.width-1,Math.max(0,Math.floor((event.clientX-rect.left)*canvas.width/rect.width))),y:Math.min(canvas.height-1,Math.max(0,Math.floor((event.clientY-rect.top)*canvas.height/rect.height)))};
  }
  const button = b => [0,2,1][b]; // browser middle/right -> protocol right/middle
  canvas.onpointerdown = event => {
    if (!enabled || event.button > 2) return;
    event.preventDefault(); canvas.focus({preventScroll:true}); canvas.setPointerCapture(event.pointerId);
    send(position(event)); send({type:'button',button:button(event.button),down:true});
  };
  canvas.onpointerup = event => {
    if (!enabled || event.button > 2) return;
    event.preventDefault(); send({type:'button',button:button(event.button),down:false});
  };
  canvas.onpointermove = event => {
    if (!enabled) return;
    motion = position(event);
    if (!animation) animation = requestAnimationFrame(() => { animation=0; if (enabled && motion) send(motion); motion=null; });
  };
  canvas.onpointercancel = disable;
  canvas.oncontextmenu = event => { if (enabled) event.preventDefault(); };
  canvas.addEventListener('wheel', wheel, {passive:false});
  function wheel(event) {
    if (!enabled) return;
    event.preventDefault();
    const scale=event.deltaMode===1?40:event.deltaMode===2?120:1;
    const bounded=value=>Math.max(-1200,Math.min(1200,Math.round(value*scale)));
    send({type:'scroll',dx:bounded(event.deltaX),dy:bounded(-event.deltaY)});
  }
  function keyboard(event) {
    if (!enabled) return;
    if (event.code==='Escape') { event.preventDefault(); disable(); return; }
    const vk=virtualKey(event.code);
    if (vk===undefined) return;
    event.preventDefault(); send({type:'key',vk,down:event.type==='keydown'});
  }
  canvas.addEventListener('keydown',keyboard); canvas.addEventListener('keyup',keyboard);
  canvas.addEventListener('blur',disable); window.addEventListener('blur',disable);
  const hidden=()=>{if(document.hidden)disable();}; document.addEventListener('visibilitychange',hidden);
  return {dispose() {
    disable(); disposed=true; checkbox.disabled=true;
    cancelAnimationFrame(animation);
    canvas.removeEventListener('wheel',wheel); canvas.removeEventListener('keydown',keyboard); canvas.removeEventListener('keyup',keyboard);
    canvas.removeEventListener('blur',disable); window.removeEventListener('blur',disable); document.removeEventListener('visibilitychange',hidden);
  }};
}

function virtualKey(code) {
  if (/^Key[A-Z]$/.test(code)) return code.charCodeAt(3);
  if (/^Digit[0-9]$/.test(code)) return code.charCodeAt(5);
  if (/^Numpad[0-9]$/.test(code)) return 0x60+Number(code[6]);
  if (/^F([1-9]|1[0-2])$/.test(code)) return 0x6f+Number(code.slice(1));
  return ({Backspace:8,Tab:9,Enter:13,NumpadEnter:13,Space:32,CapsLock:20,PageUp:33,PageDown:34,End:35,Home:36,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Insert:45,Delete:46,MetaLeft:91,MetaRight:92,ContextMenu:93,NumpadMultiply:106,NumpadAdd:107,NumpadSubtract:109,NumpadDecimal:110,NumpadDivide:111,NumLock:144,ScrollLock:145,ShiftLeft:160,ShiftRight:161,ControlLeft:162,ControlRight:163,AltLeft:164,AltRight:165,Semicolon:186,Equal:187,Comma:188,Minus:189,Period:190,Slash:191,Backquote:192,BracketLeft:219,Backslash:220,BracketRight:221,Quote:222})[code];
}
