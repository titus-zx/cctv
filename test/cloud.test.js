'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const {dispatchRequest,resolveRelay}=require('../lib/cloud');

test('dispatch contains only the device ID, platform, timestamp and signature',()=>{
  const data=dispatchRequest('12345678',1700000000);
  assert.deepEqual(Object.keys(data).sort(),['dev_id','platform','sign','timestamp']);
  assert.equal(data.dev_id,12345678);assert.equal(data.platform,10001);
  assert.equal(data.timestamp,1700000000);assert.match(data.sign,/^[0-9a-f]{40}$/);
  assert.notEqual(data.sign,dispatchRequest('12345678',1700000001).sign);
  assert.throws(()=>dispatchRequest('not-an-id'),/Device ID/);
});
test('relay lookup ignores invalid entries and falls back to a reachable server',async()=>{
  const checked=[];
  const endpoint=await resolveRelay('12345678',{
    request:async(url,options)=>{
      assert.equal(url,'http://dispa1.av380.net:8001/api/v1/get_stream_server');
      assert.equal(options.method,'POST');assert.equal(options.redirect,'error');
      assert.equal(JSON.parse(options.body).dev_id,12345678);
      return {ok:true,json:async()=>({code:2000,data:[{ip:'invalid'},{ip:'192.0.2.10'},{ip:'192.0.2.11'},{ip:'192.0.2.11'}]})};
    },
    probe:async(host,port)=>{checked.push(host);assert.equal(port,8800);return host==='192.0.2.11';},
  });
  assert.deepEqual(checked,['192.0.2.10','192.0.2.11']);assert.deepEqual(endpoint,{host:'192.0.2.11',port:8800});
});
test('dispatch service errors stop before any relay connection',async()=>{
  let probed=false;
  await assert.rejects(resolveRelay('12345678',{
    request:async()=>({ok:true,json:async()=>({code:4001,data:[]})}),
    probe:async()=>{probed=true;return true;},
  }),/belum memberikan server relay/);
  assert.equal(probed,false);
});
test('cancelling lookup interrupts an in-flight dispatch request',async()=>{
  const controller=new AbortController();let started;
  const ready=new Promise(resolve=>started=resolve);
  const task=resolveRelay('12345678',{
    signal:controller.signal,
    request:async(_url,{signal})=>new Promise((_resolve,reject)=>{
      signal.addEventListener('abort',()=>reject(new Error('aborted')),{once:true});started();
    }),
  });
  await ready;controller.abort();await assert.rejects(task,/dibatalkan/);
});
