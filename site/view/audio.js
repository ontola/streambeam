// Raw Opus -> WebCodecs -> Web Audio. Off until an explicit user click.
export async function audioPlayback(metadata) {
  const button=document.querySelector('#sound');
  const status=document.querySelector('#sound-status');
  let context,decoder,enabled=false,disposed=false,next=0,frames=0,dropped=0;
  const sources=new Set();
  const supported=metadata?.sample_rate===48000 && [1,2].includes(metadata?.channels) && 'AudioDecoder' in window && 'AudioContext' in window;
  const config={codec:'opus',sampleRate:48000,numberOfChannels:metadata?.channels};
  if (!supported || !(await AudioDecoder.isConfigSupported(config)).supported) {
    status.textContent='Sound is unavailable in this browser.';
    return {push(){},dispose(){button.disabled=true;}};
  }
  button.disabled=false;
  function mute(message='Sound off') {
    enabled=false;
    if(decoder?.state!=='closed')decoder?.close();
    for(const source of sources){source.stop();source.disconnect();} sources.clear();
    void context?.close();context=null;decoder=null;next=0;
    button.textContent='Enable sound';status.textContent=message;
    status.dataset.activeSources='0';status.dataset.enabled='false';
  }
  button.onclick=async()=>{
    if(enabled){mute();return;}
    if(disposed)return;
    // Created/resumed synchronously in the click handler to honor autoplay policy.
    context=new AudioContext({sampleRate:48000,latencyHint:'interactive'});
    const ready=context.resume();
    enabled=true;
    decoder=new AudioDecoder({
      output(data){
        try{
          if(!enabled || !context || context.state!=='running')return;
          const duration=data.numberOfFrames/data.sampleRate;
          if(data.numberOfChannels!==metadata.channels || duration>0.12)return;
          // Drop excess audio after decode (Opus state is maintained), never build
          // an unbounded backlog when a tab is throttled or the network catches up.
          const start=Math.max(context.currentTime+0.025,next);
          if(start+duration-context.currentTime>0.2 || sources.size>=24)return;
          const buffer=context.createBuffer(data.numberOfChannels,data.numberOfFrames,data.sampleRate);
          let peak=0;
          for(let channel=0;channel<data.numberOfChannels;channel++){
            const samples=buffer.getChannelData(channel);
            data.copyTo(samples,{planeIndex:channel,format:'f32-planar'});
            for(let i=0;i<samples.length;i++)peak=Math.max(peak,Math.abs(samples[i]));
          }
          const source=context.createBufferSource();source.buffer=buffer;source.connect(context.destination);
          sources.add(source);source.onended=()=>{sources.delete(source);source.disconnect();status.dataset.activeSources=String(sources.size);};
          source.start(start);next=start+duration;frames+=data.numberOfFrames;
          status.dataset.decodedFrames=String(frames);status.dataset.peak=String(peak);status.dataset.activeSources=String(sources.size);
          status.textContent='Sound on';
        }catch{mute('Sound stopped. You can enable it again.');}finally{data.close();}
      },
      error(){mute('Sound could not be decoded. Video remains available.');}
    });
    decoder.configure(config);button.textContent='Mute';status.dataset.enabled='true';
    try{await ready;}catch{mute('Browser audio could not start.');}
  };
  const hidden=()=>{if(document.hidden && enabled)mute('Sound paused while the tab is hidden.');};
  document.addEventListener('visibilitychange',hidden);
  return {
    push(packet){
      if(!enabled || !decoder)return;
      // Opus tolerates packet loss: discard excess encoded audio instead of
      // permanently muting a healthy stream after a short main-thread stall.
      if(decoder.decodeQueueSize>=12){dropped++;status.dataset.droppedPackets=String(dropped);return;}
      try{decoder.decode(new EncodedAudioChunk({type:'key',timestamp:Number(new DataView(packet.buffer,packet.byteOffset+1,8).getBigUint64(0)),data:packet.subarray(9)}));}catch{mute('Sound could not be decoded. Video remains available.');}
    },
    dispose(){mute();disposed=true;button.disabled=true;document.removeEventListener('visibilitychange',hidden);}
  };
}
