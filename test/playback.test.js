'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');const os=require('node:os');const path=require('node:path');
const {parseName,range,createStore}=require('../lib/playback');

test('segment names parse to device and local start time; other files are ignored',()=>{
  const item=parseName('cam65986431_20261014_085111.mp4',10);
  assert.equal(item.deviceId,'65986431');assert.equal(new Date(item.start).getHours(),8);assert.equal(item.size,10);
  for(const bad of ['../cam1_20261014_085111.mp4','cam1_20261014_085111.mp4.part','x.mp4'])assert.equal(parseName(bad),null);
});
test('range parsing handles open, suffix and unsatisfiable ranges',()=>{
  assert.deepEqual(range('bytes=0-9',100),{start:0,end:9});assert.deepEqual(range('bytes=90-',100),{start:90,end:99});
  assert.deepEqual(range('bytes=-10',100),{start:90,end:99});assert.equal(range('bytes=200-',100),false);assert.equal(range(undefined,100),null);
});
const {spawnSync}=require('node:child_process');
const hasRclone=!spawnSync('rclone',['version']).error;
async function read(file){let t='';for await(const c of file.stream)t+=c;return t;}
for(const destination of ['local','onedrive']){
  test(`${destination} store lists one day and serves byte ranges`,{skip:destination==='onedrive'&&!hasRclone&&'rclone tidak ada'},async()=>{
    const dir=fs.mkdtempSync(path.join(os.tmpdir(),'pb-'));
    fs.writeFileSync(path.join(dir,'cam1_20261014_080000.mp4'),'0123456789');fs.writeFileSync(path.join(dir,'cam1_20261015_080000.mp4'),'x');
    fs.writeFileSync(path.join(dir,'cam1_20261014_090000.mp4.part'),'x');
    const store=createStore({recording:{enabled:true,destination,dir,rclone:{onedrive:dir}}},os.tmpdir()); // an rclone remote may be a plain directory
    assert.deepEqual((await store.list('2026-10-14')).map(i=>i.name),['cam1_20261014_080000.mp4']);
    const file=await store.open('cam1_20261014_080000.mp4','bytes=2-5');
    assert.equal(file.status,206);assert.equal(await read(file),'2345');assert.equal(file.headers['Content-Range'],'bytes 2-5/10');
    const whole=await store.open('cam1_20261014_080000.mp4');assert.equal(whole.status,200);assert.equal(await read(whole),'0123456789');
    assert.equal(await store.open('cam1_20261014_000000.mp4'),null);
    assert.equal((await store.open('cam1_20261014_080000.mp4','bytes=50-')).status,416);
  });
}
