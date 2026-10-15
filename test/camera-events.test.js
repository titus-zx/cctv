'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const {EventEmitter}=require('node:events');
const {CameraEvents}=require('../lib/camera-events');
const {Viewer}=require('../lib/viewer');
class Response extends EventEmitter {
 constructor(){super();this.data='';this.destroyed=false;this.writableLength=0;}
 writeHead(code,headers){this.code=code;this.headers=headers;}
 flushHeaders(){}
 write(chunk){this.data+=chunk;return true;}
 end(){this.destroy();}
 destroy(){if(this.destroyed)return;this.destroyed=true;this.emit('close');}
}
const count = res => (res.data.match(/event: cameras\n/g)||[]).length;
test('SSE sends initial status, suppresses frame-only changes and broadcasts state changes',t=>{
 const camera={id:0,state:'live',frames:1,lastFrameAt:10};
 const hub=new CameraEvents(()=>[{...camera}]);t.after(()=>hub.close());
 const a=new Response();hub.subscribe(a);
 assert.equal(a.code,200);assert.match(a.headers['Content-Type'],/text\/event-stream/);
 assert.equal(count(a),1);assert.match(a.data,/retry: 2000/);
 camera.frames++;camera.lastFrameAt++;hub.publish();assert.equal(count(a),1);
 camera.state='error';hub.publish();assert.equal(count(a),2);assert.match(a.data,/"state":"error"/);
 const b=new Response();hub.subscribe(b);assert.equal(count(b),1);assert.equal(count(a),2);
 a.destroy();b.destroy();assert.equal(hub.clients.size,0);assert.equal(hub.timer,null);
});
test('audio shares SSE status connection and listeners are removed on disconnect',t=>{
 const viewer=new Viewer({deviceId:'1'});const hub=new CameraEvents(()=>[]);t.after(()=>{hub.close();viewer.stop();});
 const res=new Response();hub.subscribe(res,viewer);
 assert.equal(viewer.audioListeners.size,1);
 viewer.publishAudio(Buffer.from([0,128,255]));assert.match(res.data,/event: audio\ndata: "AID\/"/);
 res.destroy();assert.equal(viewer.audioListeners.size,0);
 const length=res.data.length;viewer.publishAudio(Buffer.from([1]));assert.equal(res.data.length,length);
});
test('slow clients are disconnected and do not keep the hub timer alive',()=>{
 const hub=new CameraEvents(()=>[]),res=new Response();hub.subscribe(res);
 res.writableLength=300000;hub.clients.values().next().value.fingerprint=null;hub.publish();
 assert.equal(res.destroyed,true);assert.equal(hub.clients.size,0);assert.equal(hub.timer,null);hub.close();
});
test('heartbeat keeps quiet SSE connections alive without camera events',t=>{
 const hub=new CameraEvents(()=>[]),res=new Response();t.after(()=>hub.close());hub.subscribe(res);
 hub.clients.values().next().value.heartbeat=0;hub.publish();
 assert.equal(count(res),1);assert.match(res.data,/: heartbeat\n\n/);
 hub.close();assert.equal(res.destroyed,true);assert.equal(hub.clients.size,0);
});
