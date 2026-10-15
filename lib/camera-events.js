'use strict';
class CameraEvents {
  constructor(snapshot) { this.snapshot=snapshot; this.clients=new Set(); this.timer=null; }
  send(client, event, data) {
    const res=client.res;
    if (res.destroyed || res.writableLength > 256 * 1024) {res.destroy();return;}
    res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  }
  publish() {
    if (!this.clients.size) return;
    const cameras=this.snapshot();
    // Frame timestamps/counters change continuously and are not displayed in the UI.
    const fingerprint=JSON.stringify(cameras.map(({frames,lastFrameAt,...status})=>status));
    for (const client of this.clients) {
      if (client.fingerprint !== fingerprint) {
        client.fingerprint=fingerprint;this.send(client,'cameras',cameras);
      }
      if (Date.now()-client.heartbeat >= 15000) {
        client.heartbeat=Date.now();
        if (!client.res.destroyed) client.res.write(': heartbeat\n\n');
      }
    }
  }
  subscribe(res, audioViewer) {
    res.writeHead(200,{'Content-Type':'text/event-stream; charset=utf-8','Cache-Control':'no-cache, no-transform',
      'Connection':'keep-alive','X-Accel-Buffering':'no','X-Content-Type-Options':'nosniff'});
    res.flushHeaders();res.write('retry: 2000\n\n');
    const client={res,fingerprint:null,heartbeat:Date.now(),unsubscribe:null};
    const remove=()=>{
      client.unsubscribe?.();this.clients.delete(client);
      if (!this.clients.size) {clearInterval(this.timer);this.timer=null;}
    };
    res.once('close',remove);
    this.clients.add(client);
    if (audioViewer) client.unsubscribe=audioViewer.subscribeAudioEvents(data=>this.send(client,'audio',data.toString('base64')));
    this.publish();
    if (!this.timer && this.clients.size) {this.timer=setInterval(()=>this.publish(),1000);this.timer.unref();}
  }
  close() {
    clearInterval(this.timer);this.timer=null;
    for (const client of this.clients) {client.unsubscribe?.();client.res.end();}
    this.clients.clear();
  }
}
module.exports={CameraEvents};
