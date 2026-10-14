'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const {Viewer}=require('../lib/viewer');

test('cloud resolution is read from JPEG SOF after application metadata',()=>{
  const viewer=new Viewer({mode:'cloud'});
  const jpeg=Buffer.from('ffd8ffe000040000ffc000110802d0050003011100021100031100ffd9','hex');
  viewer.publish(jpeg);
  assert.equal(viewer.info.width,1280);assert.equal(viewer.info.height,720);
  assert.equal(viewer.state,'live');assert.equal(viewer.lastJpeg,jpeg);
  viewer.stop();
});
test('truncated JPEG metadata does not cause a buffer read exception',()=>{
  const viewer=new Viewer({mode:'cloud'});
  viewer.publish(Buffer.from('ffd8ffc0001108','hex'));
  assert.equal(viewer.info.width,undefined);assert.equal(viewer.info.height,undefined);
  viewer.stop();
});

const {EventEmitter,once}=require('node:events');
const {spawn}=require('node:child_process');
const {V380Client}=require('../lib/v380');
const jpeg=Buffer.from('ffd8ffe000040000ffc000110801e0028003011100021100031100ffd9','hex');
function fixture(t, recorder=null) {
  const original=V380Client.prototype.start;
  V380Client.prototype.start=function(){};
  const viewer=new Viewer({deviceId:'123',password:'test'},recorder);
  t.after(()=>{viewer.stop();V380Client.prototype.start=original;});
  viewer.start();return viewer;
}
test('recording receives keyframes with no preview clients and MJPEG streams need no decoder',t=>{
  const recorder=new EventEmitter();Object.assign(recorder,{closing:new Set(),queue:{pending:[]},starts:0,writes:0,
    start(){this.starts++;this.process={};},write(){this.writes++;},stop(){this.process=null;}});
  const viewer=fixture(t,recorder);viewer.startedAt-=4000;viewer.firstFrameAt=Date.now()-4000;
  viewer.source.emit('frame',jpeg,{codec:'mjpeg',keyframe:true});
  assert.equal(recorder.starts,1);assert.equal(recorder.writes,1);
  assert.equal(viewer.state,'live');assert.ok(!viewer.decoder);
  const res=new EventEmitter();Object.assign(res,{writeHead(){},write(){},end(){},writableLength:0});
  viewer.subscribe(res);viewer.source.emit('frame',jpeg,{codec:'mjpeg',keyframe:true});
  assert.equal(viewer.lastJpeg,jpeg);assert.ok(!viewer.decoder);
});
test('recorder recovery waits for a keyframe and for the previous recorder to close',t=>{
  const recorder=new EventEmitter();Object.assign(recorder,{closing:new Set(['old']),queue:{pending:[]},starts:0,restarts:1,
    start(){this.starts++;this.process={};},write(){},stop(){}});
  const viewer=fixture(t,recorder);viewer.startedAt-=4000;viewer.firstFrameAt=Date.now()-4000;
  viewer.source.emit('frame',Buffer.from('video'),{codec:'h264',keyframe:true});assert.equal(recorder.starts,0);
  recorder.closing.clear();
  viewer.source.emit('frame',Buffer.from('video'),{codec:'h264',keyframe:false});assert.equal(recorder.starts,0);
  viewer.source.emit('frame',Buffer.from('video'),{codec:'h264',keyframe:true});assert.equal(recorder.starts,1);
});
test('a preview error leaves source and recorder running',async t=>{
  const recorder=new EventEmitter();Object.assign(recorder,{stops:0,stop(){this.stops++;}});
  const viewer=fixture(t,recorder),source=viewer.source;
  const decoder=new EventEmitter();Object.assign(decoder,{stdin:{destroy(){}},kill(){setImmediate(()=>decoder.emit('close'));}});
  viewer.decoder=decoder;viewer.state='live';
  viewer.previewFailed('preview failed',decoder,viewer.generation);
  await Promise.all([...viewer.decoderClosing]);
  assert.equal(viewer.source,source);assert.equal(recorder.stops,0);assert.equal(viewer.state,'live');
  assert.equal(viewer.previewError,'preview failed');
});
test('decoder that ignores SIGTERM is killed and reaped',async()=>{
  const viewer=new Viewer({mode:'cloud'});
  const child=spawn(process.execPath,['-e',"process.on('SIGTERM',()=>{});console.log('ready');setInterval(()=>{},1000)"],{stdio:['pipe','pipe','pipe']});
  await once(child.stdout,'data');viewer.decoder=child;
  await viewer.stopDecoder();assert.equal(child.signalCode,'SIGKILL');assert.equal(viewer.decoderClosing.size,0);
  viewer.stop();
});
test('snapshot requests a fresh MJPEG frame without spawning a decoder',async t=>{
  const viewer=fixture(t);
  const pending=viewer.snapshot();assert.equal(viewer.wantsPreview(),true);
  viewer.source.emit('frame',jpeg,{codec:'mjpeg',keyframe:true});
  assert.equal(await pending,jpeg);assert.equal(viewer.wantsPreview(),false);assert.ok(!viewer.decoder);
});

test('RTSP reconnect waits for its previous preview to close before replacing it',async()=>{
  const viewer=new Viewer({mode:'rtsp'});viewer.clients.add({end(){}});
  const decoder=new EventEmitter();Object.assign(decoder,{stdin:{destroy(){}},kill(){}});viewer.decoder=decoder;
  let opens=0;viewer.openDecoder=()=>{opens++;};
  viewer.reconnect();await new Promise(r=>setImmediate(r));assert.equal(opens,0);
  decoder.emit('close',0);await new Promise(r=>setImmediate(r));assert.equal(opens,1);
  viewer.stop();
});
