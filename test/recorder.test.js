'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');const os=require('node:os');const path=require('node:path');
const {spawnSync}=require('node:child_process');
const {DriveUploader}=require('../lib/drive');
const {Recorder,UploadQueue}=require('../lib/recorder');

test('drive upload refreshes the token, then uploads with a resumable session into the folder',async()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'drv-'));const file=path.join(dir,'cam1_20260101_000000.mp4');fs.writeFileSync(file,'video');
  const calls=[];
  const request=async(url,o)=>{calls.push({url:String(url),o});
    if(String(url).includes('oauth2'))return{ok:true,json:async()=>({access_token:'tok',expires_in:3600})};
    if(o.method==='POST')return{ok:true,status:200,headers:{get:()=>'https://upload.example/session'}};
    return{ok:true,status:200,json:async()=>({id:'file-id'})};};
  const drive=new DriveUploader({clientId:'a',clientSecret:'b',refreshToken:'c',folderId:'F'},request);
  assert.equal(await drive.upload(file),'file-id');
  assert.equal(calls[1].o.headers.Authorization,'Bearer tok');
  assert.deepEqual(JSON.parse(calls[1].o.body),{name:'cam1_20260101_000000.mp4',parents:['F']});
  assert.equal(calls[2].url,'https://upload.example/session');
  assert.throws(()=>new DriveUploader({}),/belum lengkap/);
});
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
test('onedrive folder copy is atomic and rejects relative paths',async()=>{
  const {FolderUploader}=require('../lib/folder');
  const src=fs.mkdtempSync(path.join(os.tmpdir(),'s-')),dst=path.join(fs.mkdtempSync(path.join(os.tmpdir(),'d-')),'cctv');
  const f=path.join(src,'cam1_20260101_000000.mp4');fs.writeFileSync(f,'video');
  await new FolderUploader({dir:dst}).upload(f);
  assert.deepEqual(fs.readdirSync(dst),['cam1_20260101_000000.mp4']);
  assert.throws(()=>new FolderUploader({dir:'relatif'}),/absolut/);
});
test('destination selects exactly one uploader',()=>{
  const {createRecording}=require('../lib/recorder');
  const base={enabled:true,dir:'x',drive:{clientId:'a',clientSecret:'b',refreshToken:'c'},onedrive:{dir:'/tmp/od'}};
  assert.equal(createRecording({recording:{...base,destination:'onedrive'}},os.tmpdir()).queue.uploader.constructor.name,'FolderUploader');
  assert.equal(createRecording({recording:{...base,destination:'gdrive'}},os.tmpdir()).queue.uploader.constructor.name,'DriveUploader');
  assert.throws(()=>createRecording({recording:{...base,destination:'x'}},os.tmpdir()),/destination/);
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
