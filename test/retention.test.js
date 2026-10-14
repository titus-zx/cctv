'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {Retention,retentionSettings,recordingTime}=require('../lib/retention');
const {Rclone}=require('../lib/rclone');
function fixture(t){const dir=fs.mkdtempSync(path.join(os.tmpdir(),'retention-'));t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));return dir;}
function mp4(dir,name){
 const atom=(type)=>{const b=Buffer.alloc(9);b.writeUInt32BE(9);b.write(type,4);return b;};
 fs.writeFileSync(path.join(dir,name),Buffer.concat([atom('ftyp'),atom('moov'),atom('mdat')]));
}
const now=new Date(2026,9,14,12).getTime();
const options=dir=>({days:7,checkSeconds:3600,dir,now:()=>now,cameras:()=>[{deviceId:'123'}],log:{error:()=>{}}});
test('env overrides config, blank env preserves config and zero disables retention',()=>{
 assert.deepEqual(retentionSettings({},{}),{days:0,checkSeconds:3600});
 assert.deepEqual(retentionSettings({retentionDays:7},{RECORDING_RETENTION_DAYS:'3',RECORDING_RETENTION_CHECK_SECONDS:'600'}),{days:3,checkSeconds:600});
 assert.equal(retentionSettings({retentionDays:7},{RECORDING_RETENTION_DAYS:''}).days,7);
 assert.equal(retentionSettings({retentionDays:7},{RECORDING_RETENTION_DAYS:'0'}).days,0);
 for(const bad of ['NaN','-1','Infinity'])assert.throws(()=>retentionSettings({},{RECORDING_RETENTION_DAYS:bad}));
 assert.throws(()=>retentionSettings({},{RECORDING_RETENTION_CHECK_SECONDS:'0'}));
});
test('timestamp parsing rejects traversal, invalid dates and other cameras',()=>{
 const ids=new Set(['123']);
 assert.equal(recordingTime('cam123_20260230_000000.mp4',ids),null);
 assert.equal(recordingTime('cam123_20261001_246000.mp4',ids),null);
 assert.equal(recordingTime('../cam123_20261001_000000.mp4',ids),null);
 assert.equal(recordingTime('cam999_20261001_000000.mp4',ids),null);
 assert.equal(recordingTime('cam123_20261001_000000_recovered.mp4',ids),null);
 assert.equal(recordingTime('cam123_20261001_000000.mp4',ids),new Date(2026,9,1).getTime());
});
test('local retention deletes expired completed clips and preserves boundary, active, corrupt and unrelated files',async t=>{
 const dir=fixture(t),expired='cam123_20261001_000000.mp4',boundary='cam123_20261007_120000.mp4',pending='cam123_20261002_000000.mp4';
 for(const n of [expired,boundary,pending,'cam999_20261001_000000.mp4'])mp4(dir,n);
 fs.writeFileSync(path.join(dir,'cam123_20261003_000000.mp4'),'broken');
 fs.mkdirSync(path.join(dir,'.partial-active'));mp4(path.join(dir,'.partial-active'),'cam123_20261001_000000.mp4');
 const rec=new Retention({...options(dir),queue:{queued:new Set([path.join(dir,pending)])}});
 await rec.run();assert.equal(rec.deletedLocal,1);assert.equal(fs.existsSync(path.join(dir,expired)),false);
 for(const n of [boundary,pending,'cam999_20261001_000000.mp4','cam123_20261003_000000.mp4','.partial-active'])assert.equal(fs.existsSync(path.join(dir,n)),true);
});
test('cloud retention protects pending uploads and local files absent from cloud',async t=>{
 const dir=fixture(t),expired='cam123_20261001_000000.mp4',pending='cam123_20261002_000000.mp4',unuploaded='cam123_20261003_000000.mp4';
 for(const n of [expired,pending,unuploaded])mp4(dir,n);
 const deleted=[];
 const rec=new Retention({...options(dir),queue:{queued:new Set([path.join(dir,pending)])},remote:{
  listRecordings:async()=>[expired,pending,'cam999_20261001_000000.mp4','notes.txt','cam123_20261014_000000.mp4'].map(name=>({name})),
  removeRecording:async name=>deleted.push(name),
 }});
 await rec.run();assert.deepEqual(deleted,[expired]);assert.equal(rec.deletedRemote,1);assert.equal(rec.deletedLocal,1);
 assert.equal(fs.existsSync(path.join(dir,pending)),true);assert.equal(fs.existsSync(path.join(dir,unuploaded)),true);
});
test('cleanup is serialized and remote errors do not delete local files',async t=>{
 const dir=fixture(t);mp4(dir,'cam123_20261001_000000.mp4');let release,calls=0;
 const rec=new Retention({...options(dir),remote:{listRecordings:()=>{calls++;return new Promise((_,reject)=>release=()=>reject(new Error('offline')));}}});
 const a=rec.run(),b=rec.run();assert.equal(a,b);release();await a;
 assert.equal(calls,1);assert.equal(rec.lastError,'offline');assert.equal(rec.deletedLocal,0);
 assert.equal(fs.existsSync(path.join(dir,'cam123_20261001_000000.mp4')),true);
 await rec.stop();await rec.run();assert.equal(calls,1);
});
test('rclone removes only a validated single recording and never recurses',async()=>{
 const r=new Rclone({remote:'gdrive:cctv'}),commands=[];r.run=async args=>{commands.push(args);return JSON.stringify([{Name:'cam123_20261001_000000.mp4',Path:'cam123_20261001_000000.mp4',Size:20},{Name:'cam123_20261001_000000.mp4',Path:'backup/cam123_20261001_000000.mp4',Size:20}]);};
 assert.equal((await r.listRecordings()).length,1);
 await r.removeRecording('cam123_20261001_000000.mp4');
 assert.deepEqual(commands[1],['deletefile','--','gdrive:cctv/cam123_20261001_000000.mp4']);
 await assert.rejects(r.removeRecording('../private.mp4'));
 assert.equal(commands[0].includes('--recursive'),false);
});
const hasRclone=!require('node:child_process').spawnSync('rclone',['version']).error;
test('real rclone local backend deletes only expired recordings in the destination root',{skip:!hasRclone&&'rclone tidak ada'},async t=>{
 const dir=fixture(t),local=fixture(t),old='cam123_20261001_000000.mp4',fresh='cam123_20261014_000000.mp4';
 mp4(dir,old);mp4(dir,fresh);mp4(dir,'cam999_20261001_000000.mp4');
 fs.mkdirSync(path.join(dir,'backup'));mp4(path.join(dir,'backup'),old);
 const rec=new Retention({...options(local),remote:new Rclone({remote:dir})});
 await rec.run();assert.equal(rec.lastError,null);assert.equal(rec.deletedRemote,1);
 assert.equal(fs.existsSync(path.join(dir,old)),false);
 for(const file of [fresh,'cam999_20261001_000000.mp4','backup/'+old])assert.equal(fs.existsSync(path.join(dir,file)),true);
});
