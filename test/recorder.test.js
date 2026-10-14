'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');const os=require('node:os');const path=require('node:path');
const {spawnSync}=require('node:child_process');
const {Recorder,UploadQueue}=require('../lib/recorder');

test('upload queue deletes files after success unless keepLocal',async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'q-'));const a=path.join(dir,'a.mp4'),b=path.join(dir,'b.mp4');
  fs.writeFileSync(a,'x');fs.writeFileSync(b,'x');
  const q=new UploadQueue({upload:async()=>{}},{});q.add(a);
  const k=new UploadQueue({upload:async()=>{}},{keepLocal:true});k.add(b);
  await new Promise(r=>setTimeout(r,50));
  assert.equal(fs.existsSync(a),false);assert.equal(fs.existsSync(b),true);assert.equal(q.uploaded,1);
});
test('recorder cuts a raw H.264 stream into segments and queues each one',{skip:spawnSync('ffmpeg',['-version']).error?'ffmpeg tidak ada':false},async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'rec-'));
  const gen=spawnSync('ffmpeg',['-loglevel','error','-f','lavfi','-i','testsrc=size=320x240:rate=10','-t','5','-g','10','-c:v','libx264','-f','h264','pipe:1'],{maxBuffer:1<<26});
  assert.ok(gen.stdout.length>1000);
  const seen=[];const rec=new Recorder({deviceId:'123'},{dir,segmentSeconds:2,queue:{add:f=>seen.push(f)}});
  rec.start('h264');rec.write(gen.stdout);rec.stop();
  await new Promise(r=>setTimeout(r,2500));
  assert.ok(seen.length>=1,'segment queued');
  for(const f of seen){assert.match(path.basename(f),/^cam123_\d{8}_\d{6}\.mp4$/);assert.ok(fs.statSync(f).size>0);}
});
const hasRclone=!spawnSync('rclone',['version']).error;
test('rclone uploader copies a file into the remote and drops missing files',{skip:!hasRclone&&'rclone tidak ada'},async()=>{
  const {Rclone}=require('../lib/rclone');
  const src=fs.mkdtempSync(path.join(os.tmpdir(),'s-')),dst=path.join(fs.mkdtempSync(path.join(os.tmpdir(),'d-')),'cctv');
  const f=path.join(src,'cam1_20260101_000000.mp4');fs.writeFileSync(f,'video');
  const r=new Rclone({remote:dst});await r.upload(f);
  assert.equal(fs.readFileSync(path.join(dst,'cam1_20260101_000000.mp4'),'utf8'),'video');
  await assert.rejects(r.upload(path.join(src,'hilang.mp4')),e=>e.code==='ENOENT');
  assert.throws(()=>new Rclone({}),/Remote/);
  await assert.rejects(new Rclone({remote:dst,bin:'/nonexistent/rclone'}).upload(f),/tidak ditemukan/);
});
test('destination selects the rclone remote, or local',()=>{
  const {createRecording}=require('../lib/recorder');
  const base={enabled:true,dir:'x',rclone:{gdrive:'gdrive:cctv',onedrive:'onedrive:cctv'}};
  assert.equal(createRecording({recording:{...base,destination:'onedrive'}},os.tmpdir()).queue.uploader.remote,'onedrive:cctv');
  assert.equal(createRecording({recording:{...base,destination:'gdrive'}},os.tmpdir()).queue.uploader.remote,'gdrive:cctv');
  assert.throws(()=>createRecording({recording:{...base,destination:'x'}},os.tmpdir()),/destination/);
  assert.throws(()=>createRecording({recording:{enabled:true,destination:'gdrive'}},os.tmpdir()),/Remote/);
});
test('recorder muxes A-law audio from fd 3 into the segments',{skip:spawnSync('ffmpeg',['-version']).error?'ffmpeg tidak ada':false},async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'reca-'));
  const gen=spawnSync('ffmpeg',['-loglevel','error','-f','lavfi','-i','testsrc=size=320x240:rate=10','-t','3','-g','10','-c:v','libx264','-f','h264','pipe:1'],{maxBuffer:1<<26});
  const seen=[];const rec=new Recorder({deviceId:'124'},{dir,segmentSeconds:60,queue:{add:f=>seen.push(f)}});
  rec.start('h264',{audio:true});rec.write(gen.stdout);
  for(let i=0;i<50;i++)rec.writeAudio(Buffer.concat([Buffer.alloc(16),Buffer.alloc(480,0xd5)]));
  rec.stop();await new Promise(r=>setTimeout(r,2500));
  assert.equal(seen.length,1);
  const probe=spawnSync('ffprobe',['-v','error','-show_entries','stream=codec_type','-of','csv=p=0',seen[0]]).stdout.toString();
  assert.match(probe,/video/);assert.match(probe,/audio/);
});
test('MJPEG cameras are recorded as browser-playable H.264',{skip:spawnSync('ffmpeg',['-version']).error?'ffmpeg tidak ada':false},async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'recm-'));
  const gen=spawnSync('ffmpeg',['-loglevel','error','-f','lavfi','-i','testsrc=size=320x240:rate=10','-t','2','-c:v','mjpeg','-f','mjpeg','pipe:1'],{maxBuffer:1<<26});
  const seen=[];const rec=new Recorder({deviceId:'125'},{dir,segmentSeconds:60,queue:{add:f=>seen.push(f)}});
  rec.start('mjpeg');rec.write(gen.stdout);rec.stop();await new Promise(r=>setTimeout(r,2500));
  assert.equal(seen.length,1);
  assert.match(spawnSync('ffprobe',['-v','error','-show_entries','stream=codec_name','-of','csv=p=0',seen[0]]).stdout.toString(),/h264/);
});
